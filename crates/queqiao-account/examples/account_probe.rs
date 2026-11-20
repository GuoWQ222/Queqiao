//! Checks a running queqiao-server from outside, as clients see it: the pinned certificate, logins,
//! refresh tokens (per device and program, revoked on logout), presence, and that signals only reach
//! devices of the same account; then the relay (TURN) as an attacker and as a client would use it. Needs two
//! users on the server; saves nothing on this PC.
//!
//! account_probe <host:port> <fingerprint> <user1> <password1> <user2> <password2>
//! Prints a CHECK line per check and PROBE_PASS or PROBE_FAIL.

use std::time::{Duration, Instant};

use queqiao_account::{Account, AccountEvent, DeviceIdentity, Role, SavedLogin, new_session_id};
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

fn device(label: &str) -> DeviceIdentity {
    DeviceIdentity {
        id: format!("probe-{label}-{}", new_session_id()),
        name: format!("probe {label}"),
    }
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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [server, pin, user1, password1, user2, password2] = &args[..] else {
        println!(
            "usage: account_probe <host:port> <fingerprint> <user1> <password1> <user2> <password2>"
        );
        std::process::exit(2);
    };
    let mut probe = Probe { failed: 0 };
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
    probe.check(
        "other certificate refused",
        error.contains("not the pinned one"),
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
                    Some(turn) => relay::checks(&mut probe, &turn),
                    None => probe.check("server offers a relay", false, "no TURN server"),
                },
                Err(error) => probe.check("relay credentials for a logged-in device", false, error),
            }
        }
        Err(error) => probe.check("viewer D logs in", false, error),
    }

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
