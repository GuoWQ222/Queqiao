//! Checks a running queqiao-server from outside, as clients see it: the pinned certificate, logins,
//! refresh tokens (per device and program, revoked on logout), presence, and that signals only reach
//! devices of the same account; then the relay (TURN) as an attacker and as a client would use it. Needs two
//! users on the server; saves nothing on this PC.
//!
//! account_probe <host:port> <fingerprint> <user1> <password1> <user2> <password2> [--signup-open]
//! (--signup-open: the server takes new accounts, `serve --allow-signup`; without it, new accounts must be refused).
//! Prints a CHECK line per check and PROBE_PASS or PROBE_FAIL.

use std::time::{Duration, Instant};

use queqiao_account::{
    Account, AccountEvent, ClientMessage, DeviceIdentity, Role, SavedLogin, new_session_id,
};
use queqiao_transport::Signaling;

struct Probe {
    failed: u32,
}

impl Probe {
    fn check(&mut self, name: &str, ok: bool, detail: impl std::fmt::Display) {
        println!(
            "CHECK {} {name}: {detail}",
            if ok { "ok  " } else { "FAIL" }
        );
        self.failed += u32::from(!ok);
    }
}

/// New accounts from the login form: refused unless the server takes them; then logged in at once, a taken or bad
/// name refused, and at most three a hour from one address.
fn signup(probe: &mut Probe, server: &str, pin: &str, open: bool) {
    let stamp = &new_session_id()[..8];
    let name = format!("newcomer{stamp}");
    // Each from a device of its own unless said otherwise.
    let register_from = |from: &DeviceIdentity, user: &str, password: &str| {
        Account::register(&mut login(server, pin), from, Role::Viewer, user, password)
    };
    let register = |user: &str, password: &str| register_from(&device("newcomer"), user, password);
    let first_device = device("newcomer");
    let first = register_from(&first_device, &name, "a-new-password");
    if !open {
        probe.check(
            "a new account is refused where the server does not take them",
            first
                .as_ref()
                .is_err_and(|e| e.0.contains("does not take new accounts")),
            first.err().map_or("accepted".into(), |e| e.0),
        );
        return;
    }
    probe.check(
        "a new account from the login form is logged in at once",
        first.as_ref().is_ok_and(|account| account.user == name),
        match &first {
            Ok(account) => format!("logged in as {}", account.user),
            Err(error) => error.0.clone(),
        },
    );
    drop(first);
    // That device belongs to the new account now: another account from it is refused, and nothing is made.
    let second_name = format!("second{stamp}");
    let owned = register_from(&first_device, &second_name, "a-new-password");
    probe.check(
        "a new account from a device of another account is refused",
        owned
            .as_ref()
            .is_err_and(|e| e.0.contains("belongs to another account")),
        owned.err().map_or("accepted".into(), |e| e.0),
    );
    let made = register(&second_name, "a-new-password");
    probe.check(
        "the refused one made nothing (its name is free)",
        made.is_ok(),
        made.err().map_or("free".into(), |e| e.0),
    );
    let taken = register(&name, "another-password");
    probe.check(
        "a taken user name is refused",
        taken.as_ref().is_err_and(|e| e.0.contains("taken")),
        taken.err().map_or("accepted".into(), |e| e.0),
    );
    let bad = register("x", "a-new-password");
    probe.check(
        "a bad user name is refused",
        bad.as_ref().is_err_and(|e| e.0.contains("user name has")),
        bad.err().map_or("accepted".into(), |e| e.0),
    );
    let third = register(&format!("third{stamp}"), "a-new-password");
    probe.check(
        "a third new account from one address is made",
        third.is_ok(),
        third.err().map_or("made".into(), |e| e.0),
    );
    let fourth = register(&format!("later{stamp}"), "a-new-password");
    probe.check(
        "a fourth new account from one address within the hour is refused",
        fourth
            .as_ref()
            .is_err_and(|e| e.0.contains("too many new accounts")),
        fourth.err().map_or("accepted".into(), |e| e.0),
    );
}

/// A device that lives in this probe only (its own key, pins in memory).
fn device(label: &str) -> DeviceIdentity {
    DeviceIdentity::in_memory(&format!("probe-{label}")).expect("a device key")
}

fn login(server: &str, pin: &str) -> SavedLogin {
    SavedLogin {
        server: server.to_string(),
        pin: pin.to_string(),
        refresh_token: None,
    }
}

/// The first event within `timeout` that `wanted` accepts (others are skipped).
fn wait_for(
    account: &mut Account,
    timeout: Duration,
    wanted: impl Fn(&AccountEvent) -> bool,
) -> Option<AccountEvent> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        if let Some(event) = account.next_event(left)
            && wanted(&event)
        {
            return Some(event);
        }
    }
}

/// Connections that never log in: 32 from one address may wait for their handshake, the next are closed at once
/// (without a limit, idle connections used up the server's sockets; the security review of 2026-11-22).
fn waiting_limit(probe: &mut Probe, server: &str) {
    use std::io::Read;
    use std::time::Duration;
    let Ok(address) = server.parse::<std::net::SocketAddr>() else {
        return probe.check("40 connections that do not log in", false, "not an ip:port");
    };
    let mut held: Vec<std::net::TcpStream> = (0..40)
        .filter_map(|_| std::net::TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok())
        .collect();
    std::thread::sleep(Duration::from_millis(800));
    // Closed by the server: a read ends at once (0 bytes or a reset); waiting ones time out.
    let closed = held
        .iter_mut()
        .map(|stream| {
            let _ = stream.set_read_timeout(Some(Duration::from_millis(200)));
            let mut byte = [0u8; 1];
            match stream.read(&mut byte) {
                Ok(0) => true,
                Ok(_) => false,
                Err(error) => !matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ),
            }
        })
        .filter(|closed| *closed)
        .count();
    probe.check(
        "40 connections that do not log in: 32 wait, 8 are closed at once",
        held.len() == 40 && closed == 8,
        format!("{} connected, {closed} closed", held.len()),
    );
    // Gone, so that the logins after this get their turn.
    drop(held);
    std::thread::sleep(Duration::from_secs(1));
}

/// Ten wrong passwords at once on ten connections: only 5 reach the password check (until 2026-11-22 all were
/// checked, and counted after). Run alone (`--burst`) against a server that has counted no failures for this address
/// yet, or the address's own limit refuses them first. An account name nobody has: it counts all the same.
fn burst(probe: &mut Probe, server: &str, pin: &str) {
    let unchecked = std::thread::scope(|scope| {
        let tries: Vec<_> = (0..10)
            .map(|i| {
                scope.spawn(move || {
                    Account::connect(
                        &mut login(server, pin),
                        &device(&format!("burst{i}")),
                        Role::Viewer,
                        Some(("burst-target", "guessing")),
                    )
                    .err()
                    .map(|e| e.0)
                    .unwrap_or_default()
                })
            })
            .collect();
        tries
            .into_iter()
            .map(|t| t.join().unwrap_or_default())
            .filter(|message| message.contains("too many failed logins"))
            .count()
    });
    probe.check(
        "ten wrong passwords at once: only 5 are checked",
        unchecked == 5,
        format!("{unchecked} of 10 refused before the check"),
    );
    // Once this address is locked out (wrong passwords for many names), a saved login still logs in; a password does
    // not (an address many share is not cut off from its PCs).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let name = format!("owner{stamp}");
    let owner = device("burst-owner");
    let mut saved = login(server, pin);
    let made = Account::register(&mut saved, &owner, Role::Viewer, &name, "an-owner-password");
    let made_ok = made.is_ok();
    drop(made);
    let mut locked = false;
    for i in 0..40 {
        let refused = Account::connect(
            &mut login(server, pin),
            &device(&format!("burst-other{i}")),
            Role::Viewer,
            Some((&format!("nobody{stamp}x{i}"), "guessing")),
        )
        .err()
        .map(|e| e.0)
        .unwrap_or_default();
        if refused.contains("too many failed logins") {
            locked = true;
            break;
        }
    }
    let resumed = Account::connect(&mut saved, &owner, Role::Viewer, None);
    probe.check(
        "a saved login logs in while its address is locked out",
        made_ok && locked && resumed.is_ok(),
        format!(
            "account made: {made_ok}, address locked: {locked}, saved login: {}",
            resumed
                .as_ref()
                .err()
                .map_or("accepted".into(), |e| e.0.clone())
        ),
    );
    drop(resumed);
    let with_password = Account::connect(
        &mut login(server, pin),
        &device("burst-owner2"),
        Role::Viewer,
        Some((&name, "an-owner-password")),
    );
    probe.check(
        "a password does not",
        with_password
            .as_ref()
            .err()
            .is_some_and(|e| e.0.contains("too many failed logins")),
        with_password.err().map_or("accepted".into(), |e| e.0),
    );
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let [server, pin, mode] = &args[..]
        && mode == "--burst"
    {
        let mut probe = Probe { failed: 0 };
        waiting_limit(&mut probe, server);
        burst(&mut probe, server, pin);
        println!(
            "{}",
            if probe.failed == 0 {
                "PROBE_PASS"
            } else {
                "PROBE_FAIL"
            }
        );
        std::process::exit(i32::from(probe.failed > 0));
    }
    // --accounts: only sign-up and the account hardening, against a server started anew (both need tries for this
    // address, which the other checks use up: they fail logins and codes on purpose).
    let accounts_only = args.last().is_some_and(|last| last == "--accounts");
    if accounts_only {
        args.pop();
    }
    let signup_open = args.last().is_some_and(|last| last == "--signup-open");
    if signup_open {
        args.pop();
    }
    let [server, pin, user1, password1, user2, password2] = &args[..] else {
        println!(
            "usage: account_probe <host:port> <fingerprint> <user1> <password1> <user2> <password2> [--signup-open] [--accounts]"
        );
        std::process::exit(2);
    };
    let mut probe = Probe { failed: 0 };
    if accounts_only {
        // Before the hardening checks: they lock this address out of logins (and sign-ups) for a while.
        signup(&mut probe, server, pin, signup_open);
        hardening(
            &mut probe,
            server,
            pin,
            [(user1, password1), (user2, password2)],
        );
        finish(&probe);
    }
    let (dev_a, dev_b, dev_c, dev_d) = (device("a"), device("b"), device("c"), device("d"));

    // The pinned certificate: any other fingerprint is refused before the password is sent.
    let mut wrong_pin = login(server, &"00".repeat(32));
    let result = Account::connect(
        &mut wrong_pin,
        &dev_a,
        Role::Agent,
        Some((user1, password1)),
    );
    let error = result.err().map(|e| e.0).unwrap_or_default();
    // Said plainly, with the beginnings of both fingerprints and never the server's whole one (the fingerprint to use
    // comes from whoever runs the server, not from the connection).
    probe.check(
        "other certificate refused, plainly",
        error.contains(&pin[..8]) && error.contains("00000000") && !error.contains(pin),
        &error,
    );

    // A fingerprint written with colons and capitals is the same pin.
    let spaced: String = pin
        .to_uppercase()
        .as_bytes()
        .chunks(2)
        .map(|pair| String::from_utf8_lossy(pair).into_owned())
        .collect::<Vec<_>>()
        .join(":");
    let mut colons = login(server, &spaced);
    let result = Account::connect(&mut colons, &dev_d, Role::Viewer, Some((user1, password1)));
    probe.check(
        "fingerprint with colons accepted",
        result.is_ok(),
        result.err().map_or("logged in".into(), |e| e.0),
    );

    let started = Instant::now();
    let mut bad = login(server, pin);
    let result = Account::connect(
        &mut bad,
        &dev_a,
        Role::Agent,
        Some((user1, "not the password")),
    );
    let error = result.err().map(|e| e.0).unwrap_or_default();
    probe.check(
        "wrong password refused, slowly",
        error.contains("wrong user name or password")
            && started.elapsed() >= Duration::from_millis(900),
        format!("{error} after {} ms", started.elapsed().as_millis()),
    );

    let mut login_a = login(server, pin);
    let mut agent_a =
        match Account::connect(&mut login_a, &dev_a, Role::Agent, Some((user1, password1))) {
            Ok(account) => account,
            Err(error) => {
                probe.check("agent A logs in", false, error);
                println!("PROBE_FAIL");
                std::process::exit(1);
            }
        };
    probe.check(
        "agent A logs in and gets a token",
        login_a.refresh_token.is_some(),
        &agent_a.user,
    );

    let mut login_b = login(server, pin);
    let mut viewer_b =
        match Account::connect(&mut login_b, &dev_b, Role::Viewer, Some((user1, password1))) {
            Ok(account) => account,
            Err(error) => {
                probe.check("viewer B logs in", false, error);
                println!("PROBE_FAIL");
                std::process::exit(1);
            }
        };
    let a_listed = viewer_b
        .devices
        .iter()
        .any(|d| d.id == dev_a.id && d.controllable);
    probe.check(
        "viewer B sees agent A online",
        a_listed,
        format!("{} devices", viewer_b.devices.len()),
    );

    let pushed = wait_for(
        &mut agent_a,
        Duration::from_secs(3),
        |event| matches!(event, AccountEvent::Devices(devices) if devices.iter().any(|d| d.id == dev_b.id && d.online)),
    );
    probe.check(
        "agent A is told that B came online",
        pushed.is_some(),
        "device list pushed",
    );

    // A signal between two devices of the account arrives, with the sender's identity.
    let session = new_session_id();
    let _ = viewer_b
        .session(&dev_a.id, Role::Agent, &session)
        .send("hello", "probe body");
    let delivered = wait_for(&mut agent_a, Duration::from_secs(3), |event| {
        matches!(event, AccountEvent::Signal { .. })
    });
    let ok = matches!(&delivered, Some(AccountEvent::Signal { from, from_role: Role::Viewer, session: s, kind, body, .. })
        if *from == dev_b.id && *s == session && kind == "hello" && body == "probe body");
    // The device list carries every device's key, and A can check B's signature with it.
    let b_key = agent_a
        .devices
        .iter()
        .find(|d| d.id == dev_b.id)
        .map(|d| d.public_key.clone());
    probe.check(
        "device list carries B's key",
        b_key.as_deref() == dev_b.key.as_ref().map(|k| k.public_hex()),
        b_key.unwrap_or_default(),
    );
    if let Some(AccountEvent::Signal {
        from,
        from_role,
        session: s,
        kind,
        body,
        signature,
        ..
    }) = &delivered
    {
        let checked = agent_a.check_signal(from, *from_role, s, kind, body, signature);
        probe.check(
            "B's signature checks out on A",
            checked.is_ok(),
            checked.err().map_or("signed by B's key".into(), |e| e.0),
        );
    }
    // A signal through the server signed with any other key is refused by the receiving device.
    let forger = queqiao_account::DeviceKey::generate().expect("a key");
    viewer_b.send(queqiao_account::ClientMessage::Signal {
        to: dev_a.id.clone(),
        to_role: Role::Agent,
        session: "forged".into(),
        kind: "offer".into(),
        body: "v=0 with someone else's fingerprint".into(),
        signature: forger.sign(b"anything"),
    });
    let forged = wait_for(
        &mut agent_a,
        Duration::from_secs(3),
        |event| matches!(event, AccountEvent::Signal { session, .. } if session == "forged"),
    );
    match forged {
        Some(AccountEvent::Signal {
            from,
            from_role,
            session,
            kind,
            body,
            signature,
            ..
        }) => {
            let checked =
                agent_a.check_signal(&from, from_role, &session, &kind, &body, &signature);
            probe.check(
                "a forged signature is refused by the receiving device",
                checked.is_err(),
                checked.err().map_or("accepted".into(), |e| e.0),
            );
        }
        _ => probe.check(
            "the forged signal reaches A to be refused",
            false,
            "not delivered",
        ),
    }
    probe.check(
        "signal B -> A delivered",
        ok,
        format!("{delivered:?}")
            .chars()
            .take(120)
            .collect::<String>(),
    );

    // Another account: does not see A, and cannot reach it.
    let mut login_c = login(server, pin);
    match Account::connect(&mut login_c, &dev_c, Role::Viewer, Some((user2, password2))) {
        Ok(mut viewer_c) => {
            let sees_a = viewer_c.devices.iter().any(|d| d.id == dev_a.id);
            probe.check(
                "other account does not see A",
                !sees_a,
                format!("{} devices", viewer_c.devices.len()),
            );
            let _ = viewer_c
                .session(&dev_a.id, Role::Agent, "intruder")
                .send("hello", "from another account");
            let refused = wait_for(&mut viewer_c, Duration::from_secs(3), |event| {
                matches!(event, AccountEvent::Error(_))
            });
            probe.check(
                "other account's signal refused",
                matches!(&refused, Some(AccountEvent::Error(message)) if message.contains("not online")),
                format!("{refused:?}"),
            );
            let leaked = wait_for(&mut agent_a, Duration::from_secs(1), |event| {
                matches!(event, AccountEvent::Signal { .. })
            });
            probe.check("nothing reached A", leaked.is_none(), format!("{leaked:?}"));
        }
        Err(error) => probe.check("second user logs in", false, error),
    }
    let mut taken = login(server, pin);
    let result = Account::connect(&mut taken, &dev_a, Role::Viewer, Some((user2, password2)));
    let error = result.err().map(|e| e.0).unwrap_or_default();
    probe.check(
        "other account cannot take A's device id",
        error.contains("another account"),
        &error,
    );

    // Saved login (refresh token): works for its own device and program only.
    let token_b = login_b.refresh_token.clone();
    let mut resumed_login = login_b.clone();
    match Account::connect(&mut resumed_login, &dev_b, Role::Viewer, None) {
        Ok(resumed) => {
            probe.check(
                "saved login resumes",
                resumed_login.refresh_token == token_b,
                &resumed.user,
            );
            // The newer connection of B replaces the older one, which the server closes.
            let kicked = wait_for(&mut viewer_b, Duration::from_secs(3), |event| {
                matches!(event, AccountEvent::Closed(_))
            });
            probe.check(
                "older connection of B closed",
                kicked.is_some(),
                format!("{kicked:?}"),
            );
            viewer_b = resumed;
        }
        Err(error) => probe.check("saved login resumes", false, error),
    }
    let mut stolen = SavedLogin {
        refresh_token: token_b.clone(),
        ..login(server, pin)
    };
    let result = Account::connect(&mut stolen, &dev_a, Role::Viewer, None);
    probe.check(
        "B's token refused on device A",
        result.is_err(),
        result.err().map_or("accepted".into(), |e| e.0),
    );
    let mut wrong_role = SavedLogin {
        refresh_token: login_a.refresh_token.clone(),
        ..login(server, pin)
    };
    let result = Account::connect(&mut wrong_role, &dev_a, Role::Viewer, None);
    probe.check(
        "agent token refused for the viewer",
        result.is_err(),
        result.err().map_or("accepted".into(), |e| e.0),
    );

    // Log out: the token stops working at once.
    viewer_b.send(queqiao_account::ClientMessage::Logout);
    drop(viewer_b);
    let mut after_logout = SavedLogin {
        refresh_token: token_b,
        ..login(server, pin)
    };
    let result = Account::connect(&mut after_logout, &dev_b, Role::Viewer, None);
    probe.check(
        "token revoked by logout",
        result.is_err(),
        result.err().map_or("accepted".into(), |e| e.0),
    );

    // Presence: when A leaves, the account's other devices are told.
    let mut login_d = login(server, pin);
    match Account::connect(&mut login_d, &dev_d, Role::Viewer, Some((user1, password1))) {
        Ok(mut viewer_d) => {
            drop(agent_a);
            let gone = wait_for(&mut viewer_d, Duration::from_secs(3), |event| {
                matches!(event, AccountEvent::Devices(devices)
                    if devices.iter().any(|d| d.id == dev_a.id && !d.controllable))
            });
            probe.check(
                "A shown offline after it left",
                gone.is_some(),
                "device list pushed",
            );
            // The relay, as an attacker and as a client would use it.
            let servers = viewer_d
                .session(&dev_a.id, Role::Agent, "relay-probe")
                .ice_servers();
            match servers {
                Ok(servers) => match servers.turn {
                    Some(turn) => {
                        relay::checks(&mut probe, &turn);
                        // A logout ends the program's relay credentials as well as its saved login.
                        viewer_d.send(queqiao_account::ClientMessage::Logout);
                        std::thread::sleep(Duration::from_millis(500));
                        relay::revoked(&mut probe, &turn);
                    }
                    None => probe.check("server offers a relay", false, "no TURN server"),
                },
                Err(error) => probe.check("relay credentials for a logged-in device", false, error),
            }
        }
        Err(error) => probe.check("viewer D logs in", false, error),
    }

    // Before the hardening checks, which lock this address out for a while.
    wake(
        &mut probe,
        server,
        pin,
        [(user1, password1), (user2, password2)],
    );
    help_ask(
        &mut probe,
        server,
        pin,
        [(user1, password1), (user2, password2)],
    );
    help(
        &mut probe,
        server,
        pin,
        [(user1, password1), (user2, password2)],
    );
    // Sign-up and the account hardening run apart (--accounts), against the server started anew.
    finish(&probe);
}

/// PROBE_PASS or PROBE_FAIL, and the exit code.
fn finish(probe: &Probe) -> ! {
    println!(
        "{}",
        if probe.failed == 0 {
            "PROBE_PASS"
        } else {
            "PROBE_FAIL"
        }
    );
    std::process::exit(i32::from(probe.failed > 0));
}

/// Account hardening (security point 3), from outside: new-device notices, removing devices, changing the
/// password, and the lockout after wrong passwords.
fn hardening(probe: &mut Probe, server: &str, pin: &str, users: [(&str, &str); 2]) {
    let [(user1, password1), (user2, password2)] = users;
    let wait = Duration::from_secs(3);
    let watcher = device("watch");
    let mut watcher_login = login(server, pin);
    let mut watching = match Account::connect(
        &mut watcher_login,
        &watcher,
        Role::Viewer,
        Some((user1, password1)),
    ) {
        Ok(account) => account,
        Err(error) => return probe.check("watcher logs in", false, error),
    };
    // A device the account has not had before is announced to its other devices.
    let newcomer = device("new");
    let mut newcomer_login = login(server, pin);
    let newcomer_account = Account::connect(
        &mut newcomer_login,
        &newcomer,
        Role::Agent,
        Some((user1, password1)),
    );
    let notice = wait_for(
        &mut watching,
        wait,
        |event| matches!(event, AccountEvent::DeviceJoined(name) if name.contains("probe-new")),
    );
    probe.check(
        "a new device is announced to the account",
        notice.is_some(),
        format!("{notice:?}"),
    );
    // A known device back with a new key (a password login may do that): announced too. A device of its own, so that
    // the newcomer's connection (removed below) stays.
    let keyed = device("keyed");
    let keyed_account = Account::connect(
        &mut login(server, pin),
        &keyed,
        Role::Viewer,
        Some((user1, password1)),
    );
    let _ = wait_for(&mut watching, wait, |event| {
        matches!(event, AccountEvent::DeviceJoined(_))
    });
    drop(keyed_account);
    let mut rekeyed = device("rekeyed");
    rekeyed.id = keyed.id.clone();
    let rekeyed_account = Account::connect(
        &mut login(server, pin),
        &rekeyed,
        Role::Viewer,
        Some((user1, password1)),
    );
    let changed = wait_for(&mut watching, wait, |event| {
        matches!(event, AccountEvent::DeviceKeyChanged(_))
    });
    probe.check(
        "a known device with a new key is announced to the account",
        rekeyed_account.is_ok() && changed.is_some(),
        format!("{changed:?}"),
    );
    drop(rekeyed_account);
    // Removing it disconnects it (telling it why), takes it off the list and ends its saved login.
    match newcomer_account {
        Ok(mut newcomer_account) => {
            watching.remove_device(&newcomer.id);
            let told = wait_for(
                &mut newcomer_account,
                wait,
                |event| matches!(event, AccountEvent::Error(message) if message.contains("removed")),
            );
            probe.check(
                "a removed device is disconnected and told why",
                told.is_some(),
                format!("{told:?}"),
            );
            let gone = wait_for(
                &mut watching,
                wait,
                |event| matches!(event, AccountEvent::Devices(devices) if !devices.iter().any(|d| d.id == newcomer.id)),
            );
            probe.check(
                "a removed device leaves the device list",
                gone.is_some(),
                "device list pushed",
            );
            drop(newcomer_account);
            let resumed = Account::connect(&mut newcomer_login, &newcomer, Role::Agent, None);
            probe.check(
                "a removed device's saved login is gone",
                resumed.is_err(),
                resumed.err().map_or("accepted".into(), |e| e.0),
            );
        }
        Err(error) => probe.check("a new device logs in", false, error),
    }
    // Another account's device cannot be removed.
    let other = device("other");
    let mut other_login = login(server, pin);
    let mut first = match Account::connect(
        &mut other_login,
        &other,
        Role::Viewer,
        Some((user2, password2)),
    ) {
        Ok(account) => account,
        Err(error) => return probe.check("second user logs in", false, error),
    };
    watching.remove_device(&other.id);
    let refused = wait_for(
        &mut watching,
        wait,
        |event| matches!(event, AccountEvent::Error(message) if message.contains("not a device of this account")),
    );
    probe.check(
        "removing another account's device is refused",
        refused.is_some(),
        format!("{refused:?}"),
    );
    // A password change needs the current password, revokes every saved login and closes the account's
    // other connections; the old password stops working.
    let second = device("second");
    let mut second_login = login(server, pin);
    let mut second_account = match Account::connect(
        &mut second_login,
        &second,
        Role::Viewer,
        Some((user2, password2)),
    ) {
        Ok(account) => account,
        Err(error) => return probe.check("second device of the second user logs in", false, error),
    };
    let wrong = first.change_password("not the password", "a whole new password");
    probe.check(
        "a password change needs the current password",
        wrong.is_err(),
        wrong.err().map_or("changed".into(), |e| e.0),
    );
    let new_password = format!("{password2}-changed");
    let changed = first.change_password(password2, &new_password);
    probe.check(
        "password changed, new saved login issued",
        changed.is_ok(),
        changed.err().map_or("new token".into(), |e| e.0),
    );
    let kicked = wait_for(
        &mut second_account,
        wait,
        |event| matches!(event, AccountEvent::Error(message) if message.contains("password was changed")),
    );
    probe.check(
        "the account's other connections are closed",
        kicked.is_some(),
        format!("{kicked:?}"),
    );
    drop(second_account);
    let resumed = Account::connect(&mut second_login, &second, Role::Viewer, None);
    probe.check(
        "saved logins from before the change are revoked",
        resumed.is_err(),
        resumed.err().map_or("accepted".into(), |e| e.0),
    );
    let old = Account::connect(
        &mut login(server, pin),
        &device("old"),
        Role::Viewer,
        Some((user2, password2)),
    );
    probe.check(
        "the old password no longer works",
        old.is_err(),
        old.err().map_or("accepted".into(), |e| e.0),
    );
    // After 5 wrong passwords (the wrong current password and the old one counted too), even the right
    // password is refused for a while.
    for _ in 0..4 {
        let _ = Account::connect(
            &mut login(server, pin),
            &device("guess"),
            Role::Viewer,
            Some((user2, "guessing")),
        );
    }
    let locked = Account::connect(
        &mut login(server, pin),
        &device("owner"),
        Role::Viewer,
        Some((user2, &new_password)),
    );
    probe.check(
        "after 5 wrong passwords the account name is locked",
        locked
            .as_ref()
            .err()
            .is_some_and(|e| e.0.contains("too many failed logins")),
        locked.err().map_or("accepted".into(), |e| e.0),
    );
}

/// Raw STUN/TURN against the server's relay: who may allocate, where it relays, quotas, and that it relays.
mod relay {
    use std::net::{SocketAddr, UdpSocket};
    use std::time::Duration;

    use queqiao_core::stun::{self, Class, Message, attr, method};
    use queqiao_transport::TurnServer;

    use super::Probe;

    fn id() -> [u8; 12] {
        let mut id = [0u8; 12];
        getrandom::fill(&mut id).unwrap();
        id
    }

    fn client() -> UdpSocket {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(700)))
            .unwrap();
        socket
    }

    /// Sends `bytes` (up to 3 times) and returns the first STUN reply.
    fn ask(socket: &UdpSocket, server: SocketAddr, bytes: &[u8]) -> Option<(Message, Vec<u8>)> {
        let mut buffer = vec![0u8; 2048];
        for _ in 0..3 {
            socket.send_to(bytes, server).ok()?;
            while let Ok((length, from)) = socket.recv_from(&mut buffer) {
                if from == server
                    && let Some(message) = Message::decode(&buffer[..length])
                {
                    return Some((message, buffer[..length].to_vec()));
                }
            }
        }
        None
    }

    fn code(reply: &Option<(Message, Vec<u8>)>) -> String {
        match reply {
            Some((message, _)) if message.class == Class::Success => "success".into(),
            Some((message, _)) => format!("error {}", message.error_code().unwrap_or(0)),
            None => "no answer".into(),
        }
    }

    struct Session {
        socket: UdpSocket,
        key: [u8; 16],
        realm: String,
        nonce: Vec<u8>,
        username: String,
        relayed: Option<SocketAddr>,
    }

    impl Session {
        /// Allocates with `username` / `password` after the server's 401 challenge.
        fn allocate(
            server: SocketAddr,
            username: &str,
            password: &str,
        ) -> (Session, Option<(Message, Vec<u8>)>) {
            let socket = client();
            let first = Message::new(Class::Request, method::ALLOCATE, id())
                .with(attr::REQUESTED_TRANSPORT, stun::TRANSPORT_UDP);
            let challenge = ask(&socket, server, &first.encode(None, true));
            let (realm, nonce) = match &challenge {
                Some((message, _)) => (
                    message.text(attr::REALM).unwrap_or_default(),
                    message.get(attr::NONCE).unwrap_or_default().to_vec(),
                ),
                None => (String::new(), Vec::new()),
            };
            let key = stun::long_term_key(username, &realm, password);
            let mut session = Session {
                socket,
                key,
                realm,
                nonce,
                username: username.to_string(),
                relayed: None,
            };
            let request = Message::new(Class::Request, method::ALLOCATE, id())
                .with(attr::REQUESTED_TRANSPORT, stun::TRANSPORT_UDP);
            let reply = session.signed(server, request);
            if let Some((message, _)) = &reply
                && message.class == Class::Success
            {
                session.relayed = message.address(attr::XOR_RELAYED_ADDRESS);
            }
            (session, reply)
        }

        fn signed(&self, server: SocketAddr, message: Message) -> Option<(Message, Vec<u8>)> {
            let bytes = message
                .with(attr::USERNAME, self.username.as_bytes())
                .with(attr::REALM, self.realm.as_bytes())
                .with(attr::NONCE, self.nonce.clone())
                .encode(Some(&self.key), true);
            ask(&self.socket, server, &bytes)
        }

        fn permit(&self, server: SocketAddr, peer: SocketAddr) -> Option<(Message, Vec<u8>)> {
            self.signed(
                server,
                Message::new(Class::Request, method::CREATE_PERMISSION, id())
                    .with_address(attr::XOR_PEER_ADDRESS, peer),
            )
        }

        fn release(&self, server: SocketAddr) {
            let _ = self.signed(
                server,
                Message::new(Class::Request, method::REFRESH, id())
                    .with(attr::LIFETIME, 0u32.to_be_bytes()),
            );
        }
    }

    /// After the program logged out: its credentials allocate no more.
    pub fn revoked(probe: &mut Probe, turn: &TurnServer) {
        let (_, reply) = Session::allocate(turn.address, &turn.username, &turn.password);
        probe.check(
            "relay credentials end with the logout (401)",
            code(&reply) == "error 401",
            code(&reply),
        );
    }

    pub fn checks(probe: &mut Probe, turn: &TurnServer) {
        let server = turn.address;
        let socket = client();
        let binding = ask(
            &socket,
            server,
            &Message::new(Class::Request, method::BINDING, id()).encode(None, true),
        );
        let mapped = binding
            .as_ref()
            .and_then(|(m, _)| m.address(attr::XOR_MAPPED_ADDRESS));
        probe.check(
            "STUN answers with the public address",
            mapped.is_some(),
            format!("{mapped:?}"),
        );

        let bare = Message::new(Class::Request, method::ALLOCATE, id())
            .with(attr::REQUESTED_TRANSPORT, stun::TRANSPORT_UDP);
        let reply = ask(&socket, server, &bare.encode(None, true));
        probe.check(
            "relay refused without credentials (401)",
            code(&reply) == "error 401",
            code(&reply),
        );

        let (_, reply) = Session::allocate(server, &turn.username, "not the password");
        probe.check(
            "relay refused with a wrong password",
            code(&reply) == "error 401",
            code(&reply),
        );
        let forged = match turn.username.split_once(':') {
            Some((_, device)) => format!("9999999999:{device}"),
            None => "9999999999:x".into(),
        };
        let (_, reply) = Session::allocate(server, &forged, &turn.password);
        probe.check(
            "relay refused with a forged expiry",
            code(&reply) == "error 401",
            code(&reply),
        );

        let (a, reply) = Session::allocate(server, &turn.username, &turn.password);
        let signed_ok = reply
            .as_ref()
            .is_some_and(|(_, bytes)| stun::check_integrity(bytes, &a.key));
        probe.check(
            "relay allocated with the issued credentials, answer signed",
            a.relayed.is_some() && signed_ok,
            format!("{:?}", a.relayed),
        );
        let Some(relayed_a) = a.relayed else {
            return;
        };
        let reply = a.permit(server, "127.0.0.1:9".parse().unwrap());
        probe.check(
            "no relaying to loopback (403)",
            code(&reply) == "error 403",
            code(&reply),
        );
        let account_port = SocketAddr::new(relayed_a.ip(), 47100);
        let reply = a.permit(server, account_port);
        probe.check(
            "no relaying to the server's own services (403)",
            code(&reply) == "error 403",
            code(&reply),
        );
        let unsigned = Message::new(Class::Request, method::REFRESH, id()).encode(None, true);
        let reply = ask(&a.socket, server, &unsigned);
        probe.check(
            "unsigned refresh refused (401)",
            code(&reply) == "error 401",
            code(&reply),
        );

        // Two allocations of the device relay to each other.
        let (b, _) = Session::allocate(server, &turn.username, &turn.password);
        if let Some(relayed_b) = b.relayed {
            let both = code(&a.permit(server, relayed_b)) == "success"
                && code(&b.permit(server, relayed_a)) == "success";
            let send = Message::new(Class::Indication, method::SEND, id())
                .with_address(attr::XOR_PEER_ADDRESS, relayed_b)
                .with(attr::DATA, &b"relayed hello"[..]);
            let mut delivered = None;
            for _ in 0..3 {
                let _ = a.socket.send_to(&send.encode(None, false), server);
                let mut buffer = vec![0u8; 2048];
                if let Ok((length, _)) = b.socket.recv_from(&mut buffer)
                    && let Some(message) = Message::decode(&buffer[..length])
                {
                    delivered = Some((
                        message.address(attr::XOR_PEER_ADDRESS),
                        message.get(attr::DATA).map(<[u8]>::to_vec),
                    ));
                    break;
                }
            }
            probe.check(
                "relay carries data between permitted peers",
                both && delivered == Some((Some(relayed_a), Some(b"relayed hello".to_vec()))),
                format!("{delivered:?}"),
            );
        } else {
            probe.check("second allocation", false, "none");
        }
        // Quota: four allocations per device.
        let (c, _) = Session::allocate(server, &turn.username, &turn.password);
        let (d, _) = Session::allocate(server, &turn.username, &turn.password);
        let (e, reply) = Session::allocate(server, &turn.username, &turn.password);
        probe.check(
            "fifth allocation of a device refused (486)",
            code(&reply) == "error 486",
            code(&reply),
        );
        for session in [&a, &b, &c, &d, &e] {
            session.release(server);
        }
    }
}

/// Helping someone of another account: an agent of account 2 takes help requests with a code; a viewer of
/// account 1 may signal it only after giving that code, once; wrong codes close the requests after five.
fn help(probe: &mut Probe, server: &str, pin: &str, users: [(&str, &str); 2]) {
    let [(user1, password1), (user2, password2)] = users;
    let wait = Duration::from_secs(5);
    let connect = |label: &str, role: Role, user: &str, password: &str| {
        let identity = device(label);
        let mut saved = login(server, pin);
        Account::connect(&mut saved, &identity, role, Some((user, password)))
            .map(|account| (account, identity))
    };
    let (mut helped, _helped_identity) = match connect("helped", Role::Agent, user2, password2) {
        Ok(pair) => pair,
        Err(error) => return probe.check("the helped agent logs in", false, error),
    };
    let (mut helper, _helper_identity) = match connect("helper", Role::Viewer, user1, password1) {
        Ok(pair) => pair,
        Err(error) => return probe.check("the helper logs in", false, error),
    };
    // Before any code: no signal reaches the other account.
    let session = new_session_id();
    let early = helper
        .session(&helped.device_id.clone(), Role::Agent, &session)
        .send("hello", "{}");
    let refused = wait_for(&mut helper, wait, |e| matches!(e, AccountEvent::Error(_)));
    probe.check(
        "no signal to another account without a help code",
        early.is_ok()
            && matches!(refused, Some(AccountEvent::Error(ref m)) if m.contains("not online")),
        format!("{refused:?}"),
    );
    helped.open_help("PROBECODE1");
    let help_id = match wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::HelpOpen(_))
    }) {
        Some(AccountEvent::HelpOpen(id)) => id,
        other => return probe.check("help opens with a help ID", false, format!("{other:?}")),
    };
    probe.check(
        "help opens with a 9-digit help ID",
        help_id.len() == 9 && help_id.chars().all(|c| c.is_ascii_digit()),
        &help_id,
    );
    let wrong = helper.request_help(&help_id, "WRONGCODE1");
    probe.check(
        "a wrong code is refused",
        matches!(wrong, Err(ref e) if e.0.contains("wrong help ID or code")),
        format!("{wrong:?}"),
    );
    let unknown = helper.request_help("123", "PROBECODE1");
    probe.check(
        "an unknown help ID gets the same answer",
        matches!(unknown, Err(ref e) if e.0.contains("wrong help ID or code")),
        format!("{unknown:?}"),
    );
    let granted = helper.request_help(&help_id, "PROBECODE1");
    probe.check(
        "the right code grants the helped device",
        matches!(granted, Ok(ref d) if d.id == helped.device_id && !d.public_key.is_empty()),
        format!("{granted:?}"),
    );
    let requested = wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::HelpRequested { .. })
    });
    probe.check(
        "the helped agent hears who gave the code",
        matches!(requested, Some(AccountEvent::HelpRequested { ref device, ref user }) if device.id == helper.device_id && user == user1),
        format!("{requested:?}"),
    );
    // Now a signed signal goes through and checks out on the helped side.
    let session = new_session_id();
    let sent = helper
        .session(&helped.device_id.clone(), Role::Agent, &session)
        .send("hello", "{\"probe\":1}");
    let arrived = wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::Signal { .. })
    });
    let checked = match &arrived {
        Some(AccountEvent::Signal {
            from,
            from_role,
            session,
            kind,
            body,
            signature,
            ..
        }) => helped
            .check_signal(from, *from_role, session, kind, body, signature)
            .is_ok(),
        _ => false,
    };
    probe.check(
        "after the code, the helper's signed signal arrives and checks out",
        sent.is_ok() && checked,
        format!("{arrived:?}"),
    );
    probe.check(
        "the helped side knows it is another account",
        helped.helper_account(&helper.device_id) == Some(user1),
        format!("{:?}", helped.helper_account(&helper.device_id)),
    );
    // The code was used up.
    let again = helper.request_help(&help_id, "PROBECODE1");
    probe.check("a code works once", again.is_err(), format!("{again:?}"));
    // A third device (of account 1, without a grant) still cannot reach the helped agent.
    match connect("bystander", Role::Viewer, user1, password1) {
        Ok((mut bystander, _)) => {
            let session = new_session_id();
            let _ = bystander
                .session(&helped.device_id.clone(), Role::Agent, &session)
                .send("hello", "{}");
            let refused = wait_for(&mut bystander, wait, |e| {
                matches!(e, AccountEvent::Error(_))
            });
            probe.check(
                "a grant is for the one helper device only",
                matches!(refused, Some(AccountEvent::Error(ref m)) if m.contains("not online")),
                format!("{refused:?}"),
            );
        }
        Err(error) => probe.check("a bystander logs in", false, error),
    }
    // Five wrong codes close the requests: even the right new code is refused then.
    helped.open_help("PROBECODE2");
    let _ = wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::HelpOpen(_))
    });
    for _ in 0..5 {
        let _ = helper.request_help(&help_id, "WRONGCODE2");
    }
    let closed = wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::HelpClosed(_))
    });
    let after = helper.request_help(&help_id, "PROBECODE2");
    probe.check(
        "five wrong codes close help (the agent is told)",
        matches!(closed, Some(AccountEvent::HelpClosed(_))) && after.is_err(),
        format!("{closed:?} / {after:?}"),
    );
}

/// Wake-on-LAN: an offline device of the account is woken through another agent of the account behind the same
/// address; never for another account's device, and not for one that is online.
fn wake(probe: &mut Probe, server: &str, pin: &str, users: [(&str, &str); 2]) {
    let [(user1, password1), (user2, password2)] = users;
    let wait = Duration::from_secs(5);
    let connect = |label: &str, role: Role, user: &str, password: &str| {
        let identity = device(label);
        let mut saved = login(server, pin);
        Account::connect(&mut saved, &identity, role, Some((user, password)))
    };
    let sleeper_id = match connect("sleeper", Role::Agent, user1, password1) {
        Ok(sleeper) => {
            sleeper.send(ClientMessage::DeviceInfo {
                macs: vec!["02-00-5E-00-00-51".into()],
            });
            std::thread::sleep(Duration::from_millis(500));
            sleeper.device_id.clone()
            // Dropped here: the sleeper goes offline.
        }
        Err(error) => return probe.check("the sleeper logs in", false, error),
    };
    std::thread::sleep(Duration::from_millis(500));
    let (mut waker, mut asker) = match (
        connect("waker", Role::Agent, user1, password1),
        connect("asker", Role::Viewer, user1, password1),
    ) {
        (Ok(waker), Ok(asker)) => (waker, asker),
        (waker, asker) => {
            return probe.check(
                "waker and asker log in",
                false,
                format!("{:?} / {:?}", waker.err(), asker.err()),
            );
        }
    };
    asker.send(ClientMessage::Wake {
        device_id: sleeper_id.clone(),
    });
    let request = wait_for(&mut waker, wait, |e| {
        matches!(e, AccountEvent::WakeRequest { .. })
    });
    probe.check(
        "an agent on the same network is asked to wake the offline device",
        matches!(request, Some(AccountEvent::WakeRequest { ref macs, .. }) if macs == &["02-00-5E-00-00-51".to_string()]),
        format!("{request:?}"),
    );
    let told = wait_for(&mut asker, wait, |e| {
        matches!(e, AccountEvent::Notice(_) | AccountEvent::Error(_))
    });
    probe.check(
        "the asker is told who wakes it",
        matches!(told, Some(AccountEvent::Notice(ref m)) if m.contains("sends the wake-up packet")),
        format!("{told:?}"),
    );
    asker.send(ClientMessage::Wake {
        device_id: waker.device_id.clone(),
    });
    let online = wait_for(&mut asker, wait, |e| {
        matches!(e, AccountEvent::Notice(_) | AccountEvent::Error(_))
    });
    probe.check(
        "an online device is not woken",
        matches!(online, Some(AccountEvent::Error(ref m)) if m.contains("online already")),
        format!("{online:?}"),
    );
    match connect("stranger", Role::Viewer, user2, password2) {
        Ok(mut stranger) => {
            stranger.send(ClientMessage::Wake {
                device_id: sleeper_id,
            });
            let refused = wait_for(&mut stranger, wait, |e| {
                matches!(e, AccountEvent::Notice(_) | AccountEvent::Error(_))
            });
            probe.check(
                "another account cannot wake the device",
                matches!(refused, Some(AccountEvent::Error(ref m)) if m.contains("not a device of this account")),
                format!("{refused:?}"),
            );
        }
        Err(error) => probe.check("a stranger logs in", false, error),
    }
}

/// Asking to help without a code: only while the person takes help requests; their yes grants, their no refuses.
fn help_ask(probe: &mut Probe, server: &str, pin: &str, users: [(&str, &str); 2]) {
    let [(user1, password1), (user2, password2)] = users;
    let wait = Duration::from_secs(5);
    let connect = |label: &str, role: Role, user: &str, password: &str| {
        let identity = device(label);
        let mut saved = login(server, pin);
        Account::connect(&mut saved, &identity, role, Some((user, password)))
    };
    let (Ok(mut helped), Ok(mut helper)) = (
        connect("asked", Role::Agent, user2, password2),
        connect("asker", Role::Viewer, user1, password1),
    ) else {
        return probe.check("ask: both log in", false, "");
    };
    let open = |account: &mut Account, code: &str| {
        account.open_help(code);
        match wait_for(account, wait, |e| matches!(e, AccountEvent::HelpOpen(_))) {
            Some(AccountEvent::HelpOpen(id)) => id,
            _ => String::new(),
        }
    };
    let help_id = open(&mut helped, "ASKCODE1");
    helped.close_help();
    std::thread::sleep(Duration::from_millis(300));
    let closed = helper.request_help(&help_id, "");
    probe.check(
        "an ask while help is closed gets the wrong-code answer",
        matches!(closed, Err(ref e) if e.0.contains("wrong help ID or code")),
        format!("{closed:?}"),
    );
    // Asks and answers: the person's side answers on its own thread while the helper waits.
    let ask = |helped: &mut Account, helper: &mut Account, yes: bool| {
        std::thread::scope(|scope| {
            let answering = scope.spawn(|| {
                let asked = wait_for(helped, wait, |e| {
                    matches!(e, AccountEvent::HelpAsked { .. })
                });
                if let Some(AccountEvent::HelpAsked { request, .. }) = &asked {
                    helped.answer_help(*request, yes);
                }
                asked
            });
            let result = helper.request_help(&help_id, "");
            (result, answering.join().ok().flatten())
        })
    };
    open(&mut helped, "ASKCODE2");
    let (granted, asked) = ask(&mut helped, &mut helper, true);
    probe.check(
        "an ask reaches the person there, with who asks",
        matches!(asked, Some(AccountEvent::HelpAsked { ref user, .. }) if user == user1),
        format!("{asked:?}"),
    );
    probe.check(
        "their yes grants the helped device",
        matches!(granted, Ok(ref d) if d.id == helped.device_id),
        format!("{granted:?}"),
    );
    open(&mut helped, "ASKCODE3");
    let (declined, _) = ask(&mut helped, &mut helper, false);
    probe.check(
        "their no refuses",
        matches!(declined, Err(ref e) if e.0.contains("declined")),
        format!("{declined:?}"),
    );
    // Right after a no, not again (as UU: "too frequent, please wait"): refused before the person is asked.
    open(&mut helped, "ASKCODE4");
    let again = helper.request_help(&help_id, "");
    probe.check(
        "asking again right after a no: too many requests",
        matches!(again, Err(ref e) if e.0.contains("too many requests")),
        format!("{again:?}"),
    );
    // A new help ID: shown at once while help is open; the old one no longer finds the PC.
    helped.refresh_help_id();
    let renewed = match wait_for(&mut helped, wait, |e| {
        matches!(e, AccountEvent::HelpOpen(_))
    }) {
        Some(AccountEvent::HelpOpen(id)) => id,
        _ => String::new(),
    };
    let old = helper.request_help(&help_id, "ASKCODE4");
    probe.check(
        "a new help ID: the old one no longer works",
        !renewed.is_empty()
            && renewed != help_id
            && matches!(old, Err(ref e) if e.0.contains("wrong help ID or code")),
        format!("new {renewed}, old {help_id}: {old:?}"),
    );
    // Names with a line break or a direction mark are refused (they could hide what a prompt says after them).
    let mut odd = device("odd-name");
    odd.name = "Anna\u{202E}gnp.exe".into();
    let mut saved = login(server, pin);
    let refused = Account::connect(&mut saved, &odd, Role::Viewer, Some((user1, password1)));
    probe.check(
        "a device name with a direction mark is refused",
        matches!(refused, Err(ref e) if e.0.contains("bad device id or name")),
        refused.err().map_or("accepted".into(), |e| e.0),
    );
}
