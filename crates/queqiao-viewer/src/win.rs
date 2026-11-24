//! The Windows program (docs/LINUX.md: the Linux one is written beside it).
//!
//! queqiao-viewer: the controlling side. Shows the remote screen in a window and sends mouse and
//! keyboard input made in that window.
//!
//! With an account (M3), started without options: the login form (once; the login is then saved,
//! DPAPI-encrypted), then the account's devices; double-click one to control it.
//! For tests and scripts:
//!   queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]
//!                  [--device <name or id> | --list | --logout | --ui-test <name or id>]
//!   --device connects without any window but the session's; --list prints the devices; --logout ends the
//!   saved login; --ui-test goes through the login form and the device list as a user would (filled and
//!   pressed by the program) and saves pictures of both next to the report.
//! By IP address (M2): queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>
//! Common options: [--seconds N] [--width N] [--report <file>] [--input-test] [--force <switches>]
//! --force sets test switches as QUEQIAO_FORCE does (for example network:relay).
//! --input-test sends a fixed sequence instead of the window's input: five pointer moves and a press of
//! F24 (a key no program uses); the agent run with --verify-input checks them. --version prints the
//! version.

use crate::options::*;
use crate::session::*;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, Device, DeviceIdentity, DeviceNotes, Role as AccountRole,
    SavedLogin, new_session_id,
};
use queqiao_core::text::{tr, trf};
use queqiao_display::{
    DeviceList, DeviceRow, HelpAction, HelpForm, ListAction, LoginAction, LoginFields, LoginForm,
    ask_secret, ask_text, confirm, popup_menu, show_error,
};
use queqiao_transport::{Addresses, Peer, Role, Signal, Signaling};
use windows::Win32::Foundation::HMODULE;

use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};

use windows::Win32::Graphics::Direct3D11::*;

use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};

use windows::core::Interface;

pub(crate) fn main() {
    queqiao_core::platform::install_crash_log("queqiao-viewer");
    #[cfg(feature = "test-switches")]
    if std::env::args().any(|arg| arg == "--crash-test") {
        panic!("crash test (asked for with --crash-test)");
    }
    // A terminal window (started by a session of this viewer): it runs until its command line ends.
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--terminal-pipe")
        && let Some(pipe) = args.get(2)
    {
        let title = match args.iter().position(|arg| arg == "--terminal-title") {
            Some(at) => args.get(at + 1).cloned().unwrap_or_default(),
            None => String::new(),
        };
        let test =
            cfg!(feature = "test-switches") && args.iter().any(|arg| arg == "--terminal-test");
        std::process::exit(queqiao_terminal::run_helper(pipe, &title, test));
    }
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let parsed = match args.get(1).map(String::as_str) {
        Some("--invite") => Some(invitation_options(&args[2..])),
        _ => parse(args.iter().skip(1).cloned()),
    };
    let Some(options) = parsed else {
        println!("{banner}");
        if cfg!(feature = "test-switches") {
            println!(
                "usage: queqiao-viewer [--server <host:port> --pin <fingerprint> --user <name> --password <password>]"
            );
            println!(
                "                      [--device <name or id> | --list | --logout | --ui-test <name or id>]"
            );
            println!("       queqiao-viewer --listen <ip:port> | --connect <agent-ip:port>");
            println!("       common: [--seconds N] [--width N] [--report <file>] [--input-test]");
        } else {
            println!(
                "usage: queqiao-viewer (the device list) | --invite <queqiao: link> | --open <device> | --help-connect <link>"
            );
        }
        // --version is answered by the banner; any other option is refused.
        let asked_version = args.get(1).map(String::as_str) == Some("--version");
        std::process::exit(if asked_version { 0 } else { 2 });
    };
    let mut lines = vec![banner];
    let mut failure = None;
    let passed = match run(&options, &mut lines) {
        Ok(passed) => passed,
        Err(error) => {
            lines.push(format!("ERROR {error}"));
            failure = Some(error);
            false
        }
    };
    lines.push(format!("RESULT {}", if passed { "PASS" } else { "FAIL" }));
    match &options.report {
        Some(path) => {
            let _ = std::fs::write(path, lines.join("\r\n") + "\r\n");
        }
        None => lines.iter().for_each(|line| println!("{line}")),
    }
    if let Some(error) = failure
        && options.interactive()
    {
        show_error(tr("Queqiao"), &error);
    }
    std::process::exit(if passed { 0 } else { 1 });
}

fn run(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    lines.extend(queqiao_core::platform::prepare_process());
    if !options.direct() {
        return run_account(options, lines);
    }
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));
    let mut signal = match (&options.connect, &options.listen) {
        (Some(address), _) => Signal::connect(address, Duration::from_secs(60)),
        (None, Some(address)) => Signal::listen(address, Duration::from_secs(120)),
        (None, None) => unreachable!("direct() checked it"),
    }
    .map_err(|e| e.to_string())?;
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let agent = signal.peer_address().map_err(|e| e.to_string())?;
    session(
        options,
        &device,
        &context,
        &mut signal,
        &agent.to_string(),
        Addresses::All,
        lines,
    )
}

fn run_account(options: &Options, lines: &mut Vec<String>) -> Result<bool, String> {
    let identity = match &options.test_device {
        Some(label) => DeviceIdentity::in_memory(label),
        None => DeviceIdentity::load_or_create(),
    }
    .map_err(|e| e.to_string())?;
    let mut login = SavedLogin::load(AccountRole::Viewer).unwrap_or_default();
    if let Some(server) = &options.server {
        if login.server != *server {
            login.refresh_token = None;
        }
        login.server = server.clone();
    }
    if let Some(pin) = &options.pin {
        login.pin = pin.clone();
    }
    let pictures = (options.ui_test.is_some() || options.test_submit_help).then(|| {
        options
            .report
            .as_ref()
            .and_then(|report| report.parent())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    });
    let Some(mut account) = log_in(options, &mut login, &identity, pictures.as_deref(), lines)?
    else {
        lines.push("the login form was closed".into());
        return Ok(true);
    };
    if options.test_device.is_none() {
        login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
    }
    if let Some(device) = &options.trust_new_key {
        account.trust_new_key(device).map_err(|e| e.to_string())?;
        lines.push(format!("the key of device {device} will be pinned anew"));
    }
    lines.push(format!(
        "logged in as {} at {}; this device: {} ({})",
        account.user, login.server, identity.name, identity.id
    ));
    if options.logout {
        account.send(ClientMessage::Logout);
        login.refresh_token = None;
        if options.test_device.is_none() {
            login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
        }
        lines.push("logged out: the saved login is deleted here and revoked on the server".into());
        return Ok(true);
    }
    if let Some(key) = &options.remove_device {
        let found = account
            .devices
            .iter()
            .find(|d| d.id == *key || d.name.eq_ignore_ascii_case(key))
            .map(|d| d.id.clone())
            .ok_or_else(|| format!("{key} is not on the account"))?;
        account.remove_device(&found);
        std::thread::sleep(Duration::from_millis(800));
        lines.push(format!("removed {found} from the account"));
        return Ok(true);
    }
    if options.list {
        for device in &account.devices {
            let state = if device.controllable {
                "online"
            } else {
                "offline"
            };
            let own = if device.id == account.device_id {
                ", this device"
            } else {
                ""
            };
            lines.push(format!(
                "device {} ({}): {state}{own}",
                device.name, device.id
            ));
        }
        return Ok(true);
    }
    let (device, context, gpu) = create_device()?;
    lines.push(format!("device: {gpu}"));
    if let (Some(first), Some(second)) = (&options.device, &options.also_device) {
        // Several sessions at once: each on a thread of its own, as from the device list.
        let mut running = Vec::new();
        for key in [first, second] {
            let target = wait_for_device(&mut account, key, Duration::from_secs(30))?;
            let (peer, started) = start_peer(&mut account, &target, Addresses::All)?;
            running.push((
                target.name.clone(),
                session_thread(
                    options,
                    peer,
                    started,
                    target.id.clone(),
                    target.name.clone(),
                ),
            ));
        }
        let mut passed = true;
        for (at, (name, thread)) in running.into_iter().enumerate() {
            let (result, session_lines) = thread
                .join()
                .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
            lines.extend(
                session_lines
                    .into_iter()
                    .map(|line| format!("{}| {line}", at + 1)),
            );
            match result {
                Ok(ok) => passed &= ok,
                Err(error) => {
                    passed = false;
                    lines.push(format!("{}| session with {name} failed: {error}", at + 1));
                }
            }
        }
        lines.push(format!("sessions at once: 2, all passed: {passed}"));
        return Ok(passed);
    }
    if let Some(key) = &options.device {
        let target = wait_for_device(&mut account, key, Duration::from_secs(30))?;
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::All,
            lines,
        );
    }
    if let Some(invite) = &options.invite {
        let prefill = queqiao_core::invitation::parse(invite)
            .ok_or_else(|| format!("not a Queqiao help link: {invite}"))?;
        lines.push(format!(
            "help: from the invitation link, help ID {}",
            prefill.0
        ));
        let Some(target) = ask_for_help_target(
            &mut account,
            Some(prefill),
            options,
            pictures.as_deref(),
            lines,
        ) else {
            lines.push("help: the form was closed".into());
            return Ok(!options.test_submit_help);
        };
        // A PC of another account: this PC's other addresses stay unknown to it.
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::RouteOnly,
            lines,
        );
    }
    if let Some(help_id) = &options.help_id {
        // Without --help-code: ask the person there to allow it.
        let code = options.help_code.clone().unwrap_or_default();
        let target = account
            .request_help(help_id, &code)
            .map_err(|e| e.to_string())?;
        lines.push(format!("help granted: {} ({})", target.name, target.id));
        // A PC of another account: this PC's other addresses stay unknown to it.
        return connect_to(
            &mut account,
            &target,
            options,
            &device,
            &context,
            Addresses::RouteOnly,
            lines,
        );
    }
    device_list(
        options,
        &mut account,
        &mut login,
        &identity,
        &device,
        &context,
        pictures.as_deref(),
        lines,
    )
}

/// The account connection: with the password from the options, the saved login, or the login form.
/// None when the person closed the form.
fn log_in(
    options: &Options,
    login: &mut SavedLogin,
    identity: &DeviceIdentity,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Result<Option<Account>, String> {
    let given = options.user.as_deref().zip(options.password.as_deref());
    if options.ui_test.is_none() {
        if let Some((user, password)) = given {
            let account = if options.signup {
                Account::register(login, identity, AccountRole::Viewer, user, password)
            } else {
                Account::connect(login, identity, AccountRole::Viewer, Some((user, password)))
            };
            return account.map(Some).map_err(|e| e.to_string());
        }
        if login.refresh_token.is_some() && options.user.is_none() {
            match Account::connect(login, identity, AccountRole::Viewer, None) {
                Ok(account) => return Ok(Some(account)),
                Err(error) if !options.interactive() => return Err(error.0),
                Err(error) => {
                    lines.push(format!("the saved login was refused: {error}"));
                    login.refresh_token = None;
                }
            }
        }
        if !options.interactive() {
            return Err("not logged in: give --server, --pin, --user and --password once".into());
        }
    }

    let defaults = LoginFields {
        server: login.server.clone(),
        pin: login.pin.clone(),
        user: options.user.clone().unwrap_or_default(),
        password: String::new(),
    };
    let mut form = LoginForm::new(tr("Queqiao - log in"), &defaults)
        .map_err(|e| format!("login form: {}", describe(e)))?;
    if options.ui_test.is_some() {
        form.set_fields(&LoginFields {
            password: options.password.clone().unwrap_or_default(),
            ..defaults.clone()
        });
        settle(|| form.next(20).is_some());
        if let Some(folder) = pictures {
            form.save_picture(&folder.join("login.bmp"))
                .map_err(|e| format!("login picture: {e}"))?;
        }
        form.submit();
    }
    loop {
        match form.next(15) {
            Some(LoginAction::Cancel) => return Ok(None),
            Some(action @ (LoginAction::Submit(_) | LoginAction::Register(_))) => {
                let register = matches!(action, LoginAction::Register(_));
                let (LoginAction::Submit(fields) | LoginAction::Register(fields)) = action else {
                    continue;
                };
                if [&fields.server, &fields.pin, &fields.user, &fields.password]
                    .iter()
                    .any(|value| value.is_empty())
                {
                    form.set_status(tr("Please fill in all four fields."));
                    continue;
                }
                // A new account: the password typed twice.
                if register
                    && options.ui_test.is_none()
                    && ask_secret(tr("Queqiao"), tr("The same password again:")).as_deref()
                        != Some(fields.password.as_str())
                {
                    form.set_status(tr("The two passwords are not the same."));
                    continue;
                }
                if login.server != fields.server {
                    login.refresh_token = None;
                }
                login.server = fields.server.clone();
                login.pin = fields.pin.clone();
                form.set_busy(
                    true,
                    if register {
                        tr("Creating the account...")
                    } else {
                        tr("Logging in...")
                    },
                );
                // In the background, so the form keeps responding.
                let (mut attempt, identity) = (login.clone(), identity.clone());
                let worker = std::thread::spawn(move || {
                    let result = if register {
                        Account::register(
                            &mut attempt,
                            &identity,
                            AccountRole::Viewer,
                            &fields.user,
                            &fields.password,
                        )
                    } else {
                        Account::connect(
                            &mut attempt,
                            &identity,
                            AccountRole::Viewer,
                            Some((&fields.user, &fields.password)),
                        )
                    };
                    (result, attempt)
                });
                while !worker.is_finished() {
                    if let Some(LoginAction::Cancel) = form.next(15) {
                        return Ok(None);
                    }
                }
                let (result, attempt) = worker
                    .join()
                    .map_err(|_| "the login thread failed".to_string())?;
                match result {
                    Ok(account) => {
                        *login = attempt;
                        return Ok(Some(account));
                    }
                    Err(error) if options.ui_test.is_some() => return Err(error.0),
                    Err(error) => form.set_busy(false, &error.0),
                }
            }
            None => {}
        }
    }
}

/// The device list: double-click (or select and press Connect) to control a device.
#[allow(clippy::too_many_arguments)]
fn device_list(
    options: &Options,
    account: &mut Account,
    login: &mut SavedLogin,
    identity: &DeviceIdentity,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let heading = trf(
        "Signed in as {user}. Your devices:",
        &[("user", &account.user)],
    );
    let mut list = DeviceList::new(tr("Queqiao"), &heading)
        .map_err(|e| format!("device list: {}", describe(e)))?;
    // The person's own names and favourites for the devices (kept on this PC only).
    let mut notes = DeviceNotes::load();
    list.set_rows(rows(&account.devices, &account.device_id, &notes));
    let (mut sessions, mut passed) = (0u32, true);
    let mut reconnect_at: Option<Instant> = None;
    let mut test_picked = false;
    // Sessions on their own threads: (the device's name, the thread).
    type Running = (
        String,
        std::thread::JoinHandle<(Result<bool, String>, Vec<String>)>,
    );
    let mut running: Vec<Running> = Vec::new();
    let test_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match list.next(15) {
            Some(ListAction::Closed) => break,
            Some(ListAction::More(id)) => {
                let name = account
                    .devices
                    .iter()
                    .find(|d| d.id == id)
                    .map_or(id.clone(), |d| d.name.clone());
                let favourite = notes.get(&id).favourite;
                let items = [
                    (1u16, tr("Rename..."), false),
                    (2, tr("Favourite"), favourite),
                    (3, tr("Remove from the account..."), false),
                ];
                match popup_menu(list.hwnd(), &items) {
                    Some(1) => {
                        let current = notes.get(&id).remark;
                        if let Some(remark) = ask_text(
                            tr("Queqiao"),
                            &trf("A name of your own for {name}:", &[("name", &name)]),
                            &current,
                        ) {
                            let _ = notes.set_remark(&id, &remark);
                        }
                    }
                    Some(2) => {
                        let _ = notes.set_favourite(&id, !favourite);
                    }
                    Some(3) => {
                        let question = trf(
                            "Remove {name} from the account? Its saved login ends; it needs the password again to come back.",
                            &[("name", &name)],
                        );
                        if confirm(list.hwnd(), tr("Queqiao"), &question) {
                            account.remove_device(&id);
                        }
                    }
                    _ => {}
                }
                list.set_rows(rows(&account.devices, &account.device_id, &notes));
            }
            Some(ListAction::Wake(id)) => {
                // The server's answer (who sends the packet, or why not) comes as a notice or error.
                account.send(ClientMessage::Wake { device_id: id });
            }
            Some(ListAction::Help) => {
                list.set_enabled(false);
                if let Some(target) = ask_for_help_target(account, None, options, pictures, lines) {
                    sessions += 1;
                    match connect_to(
                        account,
                        &target,
                        options,
                        device,
                        context,
                        Addresses::RouteOnly,
                        lines,
                    ) {
                        Ok(ok) => {
                            passed &= ok;
                            list.set_status(&trf(
                                "The session with {name} has ended.",
                                &[("name", &target.name)],
                            ));
                        }
                        Err(error) => {
                            passed = false;
                            list.set_status(&trf(
                                "Could not connect to {name}: {error}",
                                &[("name", &target.name), ("error", &error)],
                            ));
                        }
                    }
                }
                list.set_enabled(true);
            }
            Some(ListAction::LogOut) => {
                account.send(ClientMessage::Logout);
                login.refresh_token = None;
                if options.test_device.is_none() {
                    login.save(AccountRole::Viewer).map_err(|e| e.to_string())?;
                }
                lines.push("logged out".into());
                break;
            }
            Some(ListAction::Connect(id)) => {
                match account.devices.iter().find(|d| d.id == id).cloned() {
                    Some(target) if target.controllable && options.ui_test.is_none() => {
                        // On a thread of its own: the list stays usable, for more sessions at once.
                        match start_peer(account, &target, Addresses::All) {
                            Ok((peer, started)) => {
                                list.set_status(&trf(
                                    "Connected to {name}.",
                                    &[("name", &target.name)],
                                ));
                                running.push((
                                    target.name.clone(),
                                    session_thread(
                                        options,
                                        peer,
                                        started,
                                        target.id.clone(),
                                        target.name.clone(),
                                    ),
                                ));
                            }
                            Err(error) => {
                                passed = false;
                                lines.push(format!("session failed: {error}"));
                                list.set_status(&trf(
                                    "Could not connect to {name}: {error}",
                                    &[("name", &target.name), ("error", &error)],
                                ));
                            }
                        }
                    }
                    Some(target) if target.controllable => {
                        list.set_enabled(false);
                        list.set_status(&trf("Connected to {name}.", &[("name", &target.name)]));
                        let result = connect_to(
                            account,
                            &target,
                            options,
                            device,
                            context,
                            Addresses::All,
                            lines,
                        );
                        list.set_enabled(true);
                        sessions += 1;
                        match result {
                            Ok(ok) => {
                                passed &= ok;
                                list.set_status(&trf(
                                    "The session with {name} has ended.",
                                    &[("name", &target.name)],
                                ));
                            }
                            Err(error) => {
                                passed = false;
                                lines.push(format!("session failed: {error}"));
                                list.set_status(&trf(
                                    "Could not connect to {name}: {error}",
                                    &[("name", &target.name), ("error", &error)],
                                ));
                            }
                        }
                        if options.ui_test.is_some() {
                            break;
                        }
                    }
                    Some(target) => {
                        list.set_status(&trf(
                            "{name} is offline: Queqiao is not running there.",
                            &[("name", &target.name)],
                        ));
                    }
                    None => {}
                }
            }
            None => {}
        }
        // Sessions that ended.
        let (ended, still): (Vec<Running>, Vec<Running>) = running
            .drain(..)
            .partition(|(_, thread)| thread.is_finished());
        running = still;
        for (name, thread) in ended {
            let (result, session_lines) = thread
                .join()
                .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
            lines.extend(session_lines);
            sessions += 1;
            match result {
                Ok(ok) => {
                    passed &= ok;
                    list.set_status(&trf(
                        "The session with {name} has ended.",
                        &[("name", &name)],
                    ));
                }
                Err(error) => {
                    passed = false;
                    lines.push(format!("session failed: {error}"));
                    list.set_status(&trf(
                        "Could not connect to {name}: {error}",
                        &[("name", &name), ("error", &error)],
                    ));
                }
            }
        }
        if reconnect_at.is_none() {
            while let Some(event) = account.next_event(Duration::ZERO) {
                match event {
                    AccountEvent::Devices(devices) => {
                        list.set_rows(rows(&devices, &account.device_id, &notes))
                    }
                    AccountEvent::Closed(reason) => {
                        list.set_status(&trf(
                            "Lost the connection to the server ({reason}). Trying again...",
                            &[("reason", &reason)],
                        ));
                        reconnect_at = Some(Instant::now() + Duration::from_secs(2));
                        break;
                    }
                    AccountEvent::Error(message) | AccountEvent::Notice(message) => {
                        list.set_status(&message)
                    }
                    AccountEvent::DeviceJoined(name) => list.set_status(&trf(
                        "A new device, {name}, logged in to your account. If that was not you, change your password and take it out of the account.",
                        &[("name", &name)],
                    )),
                    AccountEvent::DeviceKeyChanged(name) => list.set_status(&trf(
                        "{name} logged in again with a new device key. If that was not you, change your password and take it out of the account.",
                        &[("name", &name)],
                    )),
                    AccountEvent::Signal { .. }
                    | AccountEvent::IceServers { .. }
                    | AccountEvent::PasswordChanged(_)
                    | AccountEvent::HelpOpen(_)
                    | AccountEvent::HelpClosed(_)
                    | AccountEvent::HelpRequested { .. }
                    | AccountEvent::HelpGranted(_)
                    | AccountEvent::HelpAsked { .. }
                    | AccountEvent::WakeRequest { .. } => {}
                }
            }
        }
        if reconnect_at.is_some_and(|at| Instant::now() >= at) {
            match Account::connect(login, identity, AccountRole::Viewer, None) {
                Ok(new) => {
                    *account = new;
                    list.set_rows(rows(&account.devices, &account.device_id, &notes));
                    list.set_status(tr("Connected to the server again."));
                    reconnect_at = None;
                }
                Err(error) => {
                    list.set_status(&trf(
                        "The server cannot be reached ({error}). Trying again...",
                        &[("error", &error.to_string())],
                    ));
                    reconnect_at = Some(Instant::now() + Duration::from_secs(5));
                }
            }
        }
        // The UI test: select the device once it is online and press Connect, as a person would.
        if let Some(key) = &options.ui_test
            && !test_picked
        {
            let online = account
                .devices
                .iter()
                .any(|d| (d.id == *key || d.name.eq_ignore_ascii_case(key)) && d.controllable);
            if online && list.select(key) {
                settle(|| list.next(20).is_some());
                if let Some(folder) = pictures {
                    list.save_picture(&folder.join("devices.bmp"))
                        .map_err(|e| format!("device list picture: {e}"))?;
                }
                list.press_connect();
                test_picked = true;
            } else if Instant::now() > test_deadline {
                return Err(format!("{key} did not come online within 30 s"));
            }
        }
    }
    lines.push(format!("{sessions} sessions from the device list"));
    // The list was closed: sessions still open end when their windows do.
    for (name, thread) in running {
        let (result, session_lines) = thread
            .join()
            .unwrap_or_else(|_| (Err("the session's thread failed".into()), Vec::new()));
        lines.extend(session_lines);
        sessions += 1;
        if let Err(error) = &result {
            lines.push(format!("session with {name} failed: {error}"));
        }
        passed &= result.unwrap_or(false);
    }
    Ok(passed && (options.ui_test.is_none() || sessions == 1))
}

/// Lets a form paint for about 200 ms before its picture is taken; `step` handles messages.
fn settle(mut step: impl FnMut() -> bool) {
    for _ in 0..10 {
        let _ = step();
    }
}

/// The device list's rows: devices that can be controlled first, then by name.
/// The device list's rows: favourites first, then those that can be controlled, then by name.
fn rows(devices: &[Device], own: &str, notes: &DeviceNotes) -> Vec<DeviceRow> {
    let mut sorted: Vec<&Device> = devices.iter().collect();
    sorted.sort_by_key(|d| {
        (
            !notes.get(&d.id).favourite,
            !d.controllable,
            notes.label(&d.id, &d.name).to_lowercase(),
        )
    });
    sorted
        .into_iter()
        .map(|d| DeviceRow {
            id: d.id.clone(),
            name: notes.label(&d.id, &d.name),
            status: format!(
                "{}{}",
                tr(if d.controllable { "Online" } else { "Offline" }),
                if d.id == own {
                    format!(" - {}", tr("this PC"))
                } else {
                    String::new()
                }
            ),
        })
        .collect()
}

/// Waits until the device (id or name, any case) can be controlled.
fn wait_for_device(account: &mut Account, key: &str, timeout: Duration) -> Result<Device, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let found = account
            .devices
            .iter()
            .find(|d| (d.id == key || d.name.eq_ignore_ascii_case(key)) && d.controllable);
        if let Some(device) = found {
            return Ok(device.clone());
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let known: Vec<&str> = account.devices.iter().map(|d| d.name.as_str()).collect();
            return Err(format!(
                "{key} is not online (devices on the account: {})",
                known.join(", ")
            ));
        }
        if let Some(AccountEvent::Closed(reason)) =
            account.next_event(left.min(Duration::from_millis(200)))
        {
            return Err(reason);
        }
    }
}

/// A session with the agent on `target`, set up through the server.
/// The "Help someone..." dialog: the help ID and code, until the server grants them (or the person gives up).
/// Filled in from an invitation link, or else from an invitation on the clipboard; Connect stays the person's click.
fn ask_for_help_target(
    account: &mut Account,
    prefill: Option<(String, String)>,
    options: &Options,
    pictures: Option<&Path>,
    lines: &mut Vec<String>,
) -> Option<Device> {
    let mut form = HelpForm::new().ok()?;
    let clipboard = || {
        queqiao_input::clipboard_text(None)
            .as_deref()
            .and_then(queqiao_core::invitation::parse)
    };
    match prefill {
        Some((help_id, code)) => {
            form.fill(&help_id, &code);
            form.set_status(tr(
                "Filled in from the invitation link: check it and press Connect.",
            ));
        }
        None => {
            if let Some((help_id, code)) = clipboard() {
                form.fill(&help_id, &code);
                form.set_status(tr(
                    "Filled in from the clipboard: check it and press Connect.",
                ));
                lines.push(format!(
                    "help: filled in from the clipboard, help ID {help_id}"
                ));
            }
        }
    }
    if options.test_submit_help {
        settle(|| form.next(20).is_some());
        if let Some(folder) = pictures {
            let _ = form.save_picture(&folder.join("help_form.bmp"));
        }
        form.press_connect();
    } else if options.help_go {
        settle(|| form.next(20).is_some());
        form.press_connect();
    }
    loop {
        match form.next(50) {
            Some(HelpAction::Submit { help_id, code }) => {
                match account.request_help(&help_id, &code) {
                    Ok(target) => {
                        lines.push(format!("help granted: {} ({})", target.name, target.id));
                        return Some(target);
                    }
                    Err(error) if options.test_submit_help => {
                        lines.push(format!("help refused: {error}"));
                        return None;
                    }
                    Err(error) => form.set_status(&error.0),
                }
            }
            Some(HelpAction::Cancel) => return None,
            None => {}
        }
    }
}

fn connect_to(
    account: &mut Account,
    target: &Device,
    options: &Options,
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    addresses: Addresses,
    lines: &mut Vec<String>,
) -> Result<bool, String> {
    let id = new_session_id();
    let mut signal = account.session(&target.id, AccountRole::Agent, &id);
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let (began, since) = (queqiao_account::connections::local_now(), Instant::now());
    let result = session(
        options,
        device,
        context,
        &mut signal,
        &target.name,
        addresses,
        lines,
    );
    if result.is_ok() {
        remember(&target.id, &target.name, began, since);
    }
    result
}

/// Notes a session this PC started, once it ended (the main window lists them as "Recent connections").
fn remember(device_id: &str, device_name: &str, began: String, since: Instant) {
    queqiao_account::connections::record(&queqiao_account::connections::Connection {
        when: began,
        seconds: since.elapsed().as_secs(),
        device_id: device_id.to_string(),
        device_name: device_name.to_string(),
    });
}

/// Connects to `target` through the server: the hellos, offer, answer and candidates (signalling needs the account,
/// so this is on its thread). The peer and when it started.
fn start_peer(
    account: &mut Account,
    target: &Device,
    addresses: Addresses,
) -> Result<(Peer, Instant), String> {
    let id = new_session_id();
    let mut signal = account.session(&target.id, AccountRole::Agent, &id);
    signal.hello("viewer").map_err(|e| e.to_string())?;
    let started = Instant::now();
    let peer = Peer::start_with(Role::Viewer, &mut signal, addresses).map_err(|e| e.to_string())?;
    Ok((peer, started))
}

/// A session on a thread of its own, with a graphics device of its own (several run at once): its outcome and
/// report lines when it ends.
fn session_thread(
    options: &Options,
    peer: Peer,
    started: Instant,
    device_id: String,
    agent: String,
) -> std::thread::JoinHandle<(Result<bool, String>, Vec<String>)> {
    let options = options.clone();
    std::thread::spawn(move || {
        let mut lines = Vec::new();
        let began = queqiao_account::connections::local_now();
        let result = create_device().and_then(|(device, context, _)| {
            run_session(
                &options, &device, &context, peer, started, &agent, &mut lines,
            )
        });
        if result.is_ok() {
            remember(&device_id, &agent, began, started);
        }
        (result, lines)
    })
}

/// The GPU's D3D11 device with video support: the GPU showing the primary screen first (the window opens there; no
/// picture crosses between GPUs), then the others discrete first (the user's choice, 2026-11-23); Microsoft's
/// software adapter (WARP) without a usable GPU or when forced (QUEQIAO_FORCE=adapter:warp).
pub(crate) fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext, String), String> {
    let forced_warp = queqiao_core::forced("adapter").as_deref() == Some("warp");
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let mut failures = Vec::new();
    if !forced_warp {
        for (adapter, name) in queqiao_capture::gpus_screen_first() {
            let mut device = None;
            let mut context = None;
            let created = unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    video,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )
            };
            match (created, device, context) {
                (Ok(()), Some(device), Some(context)) => return Ok((device, context, name)),
                (Err(error), _, _) => failures.push(format!("{name}: {}", describe(error))),
                _ => failures.push(format!("{name}: nothing returned")),
            }
        }
    }
    let video = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
    let attempts: &[(D3D_DRIVER_TYPE, D3D11_CREATE_DEVICE_FLAG)] = if forced_warp {
        &[(D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT)]
    } else {
        &[
            (D3D_DRIVER_TYPE_HARDWARE, video),
            (D3D_DRIVER_TYPE_WARP, D3D11_CREATE_DEVICE_BGRA_SUPPORT),
        ]
    };
    for &(driver, flags) in attempts {
        let mut device = None;
        let mut context = None;
        let created = unsafe {
            D3D11CreateDevice(
                None::<&IDXGIAdapter>,
                driver,
                HMODULE::default(),
                flags,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
        };
        match (created, device, context) {
            (Ok(()), Some(device), Some(context)) => {
                let name = adapter_name(&device);
                return Ok((device, context, name));
            }
            (Err(error), _, _) => failures.push(describe(error)),
            _ => failures.push("nothing returned".into()),
        }
    }
    Err(format!("no D3D11 device: {}", failures.join("; ")))
}

fn adapter_name(device: &ID3D11Device) -> String {
    device
        .cast::<IDXGIDevice>()
        .and_then(|d| unsafe { d.GetAdapter() })
        .and_then(|a| unsafe { a.GetDesc() })
        .map(|desc| {
            let text = &desc.Description;
            let len = text.iter().position(|&c| c == 0).unwrap_or(text.len());
            String::from_utf16_lossy(&text[..len])
        })
        .unwrap_or_else(|_| "unknown".into())
}

pub(crate) fn describe(error: windows::core::Error) -> String {
    format!("0x{:08X} {}", error.code().0, error.message())
}
