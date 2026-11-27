//! queqiao-server: accounts, devices, presence, and the signaling that lets an account's devices connect
//! to each other (TLS WebSockets), plus STUN and the TURN relay (UDP) for devices that cannot reach each
//! other directly.
//!
//! queqiao-server serve [--listen 0.0.0.0:47100] [--data <folder>] [--name <host name or IP>]...
//!                      [--public-ip <IP>] [--turn-port 3478] [--relay-ports 47200-47399] [--relay-private]
//!     Starts the server. On first start it makes a self-signed certificate for the given names and prints
//!     its SHA-256 fingerprint, which clients pin (--pin). STUN/TURN listen on UDP --turn-port (0: off) and
//!     need the address clients reach this server at (--public-ip, or the first --name that is an IP);
//!     relayed addresses use --relay-ports. --relay-private also relays to private and 100.64/10
//!     addresses (tests, LAN-only servers).
//! queqiao-server init [--data <folder>] [--name <host name or IP>]...
//!     Makes the certificate if there is none yet and prints its fingerprint.
//! queqiao-server add-user <name> [--data <folder>]
//!     Adds a user; the password is one line on standard input (never the command line, where process lists and
//!     shell history show it).
//! queqiao-server remove-user <name> [--data <folder>]
//!     Takes a user out, with their devices and logins.
//!
//! Several relays (the user, 2026-11-26: spread the relayed traffic over the servers): the account server hands out
//! its own relay and those of the relay servers that joined it, each session the one nearest to both ends with room.
//! queqiao-server add-relay <name> [--data <folder>]
//!     On the account server: lets relay <name> join; prints its key ("name:key") once, for `relay-key` there.
//! queqiao-server remove-relay <name> [--data <folder>] / queqiao-server relays [--data <folder>]
//! queqiao-server relay-key [--data <folder>]
//!     On a relay server: keeps the key (one line on standard input, never the command line) in relay.key.
//! queqiao-server relay --central <host:port> --pin <sha256> --public-ip <IP> [--data <folder>] [--turn-port 3478]
//!                      [--relay-ports 47200-47399] [--capacity-mbps 100] [--standby [--accounts-port 47100]]
//!     Runs STUN/TURN only, joined to the account server at --central (its certificate pinned). More than one
//!     --central/--pin: the first that takes it (the account server, then a standby made the account server).
//!     --standby: also keeps a live copy of the accounts in its folder; `serve` on that folder makes it the account
//!     server (stop the relay first, and the old account server if it still runs).
//! --capacity-mbps (serve and relay): what the server may send in all; nearly full, it shares that fairly.
//! queqiao-server punch [on|off] [--rounds <n>] [--reports on|off] [--data <folder>]
//!     Punching for the sessions from now on (as UU Remote's server switches its own): on or off, the most rounds a
//!     relayed session punches (0: no limit), whether the sessions' anonymous path reports are collected (on until set).
//! queqiao-server path-stats [--days 30] [--data <folder>]
//!     Those reports summed: per pair of NAT kinds, how many sessions went direct, how, how long it took. A report holds
//!     only NAT kinds, the method and a duration; the server keeps counts by day (90 days), never who sent one.
//! Without arguments it prints its version.

mod cert;
mod hub;
mod link;
mod relays;
mod replica;
mod store;
mod turn;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use store::Store;

/// A server's capacity when none is given (the user's servers: 100 to 200 Mbit/s each, 2026-11-26).
const DEFAULT_CAPACITY_MBPS: f64 = 100.0;

fn main() {
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("add-user") => add_user(&args[1..]),
        Some("remove-user") => remove_user(&args[1..]),
        Some("remove-device") => remove_device(&args[1..]),
        Some("init") => init(&args[1..]),
        Some("add-relay") => add_relay(&args[1..]),
        Some("remove-relay") => remove_relay(&args[1..]),
        Some("relays") => list_relays(&args[1..]),
        Some("relay-key") => relay_key(&args[1..]),
        Some("relay") => relay(&args[1..]),
        Some("punch") => punch(&args[1..]),
        Some("path-stats") => path_stats(&args[1..]),
        _ => {
            println!("{banner}");
            println!(
                "usage: queqiao-server serve [--listen 0.0.0.0:47100] [--data <folder>] [--name <host or IP>] [--allow-signup]..."
            );
            println!(
                "                            [--public-ip <IP>] [--turn-port 3478] [--relay-ports 47200-47399] [--relay-private]"
            );
            println!(
                "                            [--capacity-mbps 100] [--relay-name <its relay's place>]"
            );
            println!("       queqiao-server init [--data <folder>] [--name <host or IP>]...");
            println!(
                "       queqiao-server add-user <name> [--data <folder>]   (the password on standard input)"
            );
            println!("       queqiao-server remove-user <name> [--data <folder>]");
            println!("       queqiao-server remove-device <user> <device id> [--data <folder>]");
            println!(
                "       queqiao-server add-relay <name> | remove-relay <name> | relays   [--data <folder>]"
            );
            println!(
                "       queqiao-server relay-key [--data <folder>]   (the key add-relay printed, on standard input)"
            );
            println!(
                "       queqiao-server relay --central <host:port> --pin <sha256> --public-ip <IP> [--data <folder>]"
            );
            println!(
                "                            [--turn-port 3478] [--relay-ports 47200-47399] [--capacity-mbps 100] [--standby]"
            );
            println!(
                "       queqiao-server punch [on|off] [--rounds <n, 0: no limit>] [--reports on|off] [--data <folder>]"
            );
            println!("       queqiao-server path-stats [--days 30] [--data <folder>]");
            return;
        }
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

/// The value after `flag` in `args`, for each time it appears.
fn values(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect()
}

fn data_folder(args: &[String]) -> Result<PathBuf, String> {
    let folder = values(args, "--data")
        .pop()
        .map_or_else(|| PathBuf::from("queqiao-server-data"), PathBuf::from);
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    // Only its owner may read or enter it: the server's private key and the database are in it (on Linux a new folder
    // is usually readable by every user; the security review of 2026-11-22).
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", folder.display()))?;
    }
    Ok(folder)
}

fn add_user(args: &[String]) -> Result<(), String> {
    let usage = "usage: queqiao-server add-user <name> [--data <folder>], the password as one line on standard input";
    let Some(name) = args.first().filter(|name| !name.starts_with("--")) else {
        return Err(usage.into());
    };
    // Never on the command line, where process lists and shell history show it (the security review of 2026-11-22).
    if args.get(1).is_some_and(|next| !next.starts_with("--")) {
        return Err(format!(
            "{usage} (a password on the command line is refused)"
        ));
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprint!("password for {name}: ");
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let password = line.trim_end_matches(['\r', '\n']);
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    store.add_user(name, password)?;
    println!("user {name} added");
    Ok(())
}

/// Takes a user out with their devices and logins (a running server forgets them at its next check of a login).
fn remove_user(args: &[String]) -> Result<(), String> {
    let Some(name) = args.first().filter(|name| !name.starts_with("--")) else {
        return Err("usage: queqiao-server remove-user <name> [--data <folder>]".into());
    };
    let mut store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    if store.remove_user(name)? {
        println!("user {name} removed");
        Ok(())
    } else {
        Err(format!("no user {name}"))
    }
}

/// Removes one device of a user (with its saved logins), as its owner's "Remove" in the device list does. A running
/// server shows the shorter list from the account's next change.
fn remove_device(args: &[String]) -> Result<(), String> {
    let usage = "usage: queqiao-server remove-device <user> <device id> [--data <folder>]";
    let mut plain = args.iter().take_while(|arg| !arg.starts_with("--"));
    let (Some(user), Some(device)) = (plain.next(), plain.next()) else {
        return Err(usage.into());
    };
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    let record = store.password_record(user)?;
    if record.id < 0 {
        return Err(format!("no user {user}"));
    }
    if store.remove_device(record.id, device)? {
        println!("device {device} of {user} removed");
        Ok(())
    } else {
        Err(format!("{user} has no device {device}"))
    }
}

fn init(args: &[String]) -> Result<(), String> {
    let (_, fingerprint) = cert::load_or_create(&data_folder(args)?, &values(args, "--name"))?;
    println!("certificate fingerprint: {fingerprint}");
    Ok(())
}

/// The first argument when it is not an option.
fn plain_argument<'a>(args: &'a [String], usage: &str) -> Result<&'a str, String> {
    args.first()
        .filter(|name| !name.starts_with("--"))
        .map(String::as_str)
        .ok_or_else(|| usage.to_string())
}

/// Punching for every session from now on (a running server reads it at each session's relays): on or off, the most
/// rounds a relayed session punches, and whether the sessions' anonymous path reports are collected. Prints the
/// settings.
fn punch(args: &[String]) -> Result<(), String> {
    let usage = "usage: queqiao-server punch [on|off] [--rounds <n, 0: no limit>] [--reports on|off] [--data <folder>]";
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    let mut settings = store.punch_settings()?;
    let switch = |word: &str| match word {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(usage.to_string()),
    };
    if let Some(word) = args.first().filter(|word| !word.starts_with("--")) {
        settings.enabled = switch(word)?;
    }
    if let Some(rounds) = values(args, "--rounds").pop() {
        settings.rounds = rounds.parse().map_err(|_| usage.to_string())?;
    }
    if let Some(word) = values(args, "--reports").pop() {
        settings.reports = switch(&word)?;
    }
    store.set_punch_settings(&settings)?;
    println!(
        "punching {}, rounds {}, path reports {}",
        if settings.enabled { "on" } else { "off" },
        match settings.rounds {
            0 => "without limit".to_string(),
            rounds => format!("at most {rounds}"),
        },
        if settings.reports {
            "collected"
        } else {
            "not collected"
        }
    );
    Ok(())
}

/// The sessions' anonymous path reports of the last days, summed: per pair of NAT kinds (the viewer's, the agent's),
/// how many went direct and how, and how long that took.
fn path_stats(args: &[String]) -> Result<(), String> {
    let days: i64 = match values(args, "--days").pop() {
        Some(days) => days
            .parse()
            .ok()
            .filter(|days| *days > 0)
            .ok_or("usage: queqiao-server path-stats [--days 30] [--data <folder>]")?,
        None => 30,
    };
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    let rows = store.path_stats(days)?;
    let total: i64 = rows.iter().map(|row| row.4).sum();
    let direct: i64 = rows
        .iter()
        .filter(|row| row.2 != "relay")
        .map(|row| row.4)
        .sum();
    println!("sessions of the last {days} days: {total}, direct {direct}");
    println!(
        "{:<9} {:<9} {:<13} {:<14} {:>8}",
        "viewer", "agent", "how", "to direct", "sessions"
    );
    for (local, remote, method, bucket, count) in rows {
        println!("{local:<9} {remote:<9} {method:<13} {bucket:<14} {count:>8}");
    }
    Ok(())
}

/// Lets relay `name` join; its key goes to standard output only, once, as "name:key" for `relay-key`.
fn add_relay(args: &[String]) -> Result<(), String> {
    let name = plain_argument(
        args,
        "usage: queqiao-server add-relay <name> [--data <folder>]",
    )?;
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    let key = store.add_relay(name)?;
    eprintln!(
        "relay {name} may join; give the next line to `queqiao-server relay-key` on it, and to nobody else:"
    );
    println!("{name}:{}", store::hex(&key));
    Ok(())
}

fn remove_relay(args: &[String]) -> Result<(), String> {
    let name = plain_argument(
        args,
        "usage: queqiao-server remove-relay <name> [--data <folder>]",
    )?;
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    if store.remove_relay(name)? {
        println!("relay {name} removed");
        Ok(())
    } else {
        Err(format!("no relay {name}"))
    }
}

fn list_relays(args: &[String]) -> Result<(), String> {
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    for (name, created) in store.relays()? {
        println!("{name} (added {created})");
    }
    Ok(())
}

/// The key `add-relay` printed, from standard input into relay.key (its owner's only).
fn relay_key(args: &[String]) -> Result<(), String> {
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        eprint!("relay key (name:key): ");
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let (name, _) = link::parse_key_line(&line)?;
    let path = data_folder(args)?.join("relay.key");
    write_private(&path, line.trim().as_bytes())?;
    println!("relay {name}: key kept in {}", path.display());
    Ok(())
}

/// Writes a file only its owner may read (on Linux; the data folder is its owner's only anyway).
fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    file.write_all(data)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The capacity option (Mbit/s), DEFAULT_CAPACITY_MBPS without it.
fn capacity_mbps(args: &[String]) -> Result<f64, String> {
    match values(args, "--capacity-mbps").pop() {
        Some(value) => value
            .parse::<f64>()
            .ok()
            .filter(|mbps| mbps.is_finite() && *mbps >= 0.0)
            .ok_or_else(|| "--capacity-mbps: a number of Mbit/s (0: no sharing)".to_string()),
        None => Ok(DEFAULT_CAPACITY_MBPS),
    }
}

/// A relay server: STUN/TURN with the key the account server gave it, joined to that server.
fn relay(args: &[String]) -> Result<(), String> {
    let folder = data_folder(args)?;
    // The account server and its standbys, in order, each --central with the --pin after it.
    let centrals: Vec<(String, String)> = values(args, "--central")
        .into_iter()
        .zip(values(args, "--pin"))
        .collect();
    if centrals.is_empty() || centrals.len() != values(args, "--central").len() {
        return Err(
            "relay: --central <host:port> --pin <its certificate fingerprint>, for each account server".into(),
        );
    }
    let central = centrals
        .iter()
        .map(|(address, _)| address.as_str())
        .collect::<Vec<_>>()
        .join(", then ");
    let Some(public_ip) = values(args, "--public-ip").pop() else {
        return Err("relay: --public-ip <the address clients reach this server at>".into());
    };
    // A standby keeps a copy of the accounts in its folder, and the certificate it would serve them with.
    let standby = if args.iter().any(|arg| arg == "--standby") {
        let (_, pin) = cert::load_or_create(&folder, std::slice::from_ref(&public_ip))?;
        let port = values(args, "--accounts-port")
            .pop()
            .unwrap_or_else(|| "47100".into());
        let accounts = format!("{public_ip}:{port}");
        accounts
            .parse::<SocketAddr>()
            .map_err(|e| format!("--accounts-port: {e}"))?;
        println!(
            "standby: a copy of the accounts in {}; would serve them at {accounts}, certificate {pin}",
            folder.display()
        );
        Some(link::StandbySide {
            store: std::sync::Mutex::new(Store::open(&folder.join("queqiao.db"))?),
            accounts,
            pin,
        })
    } else {
        None
    };
    let path = folder.join("relay.key");
    let line = std::fs::read_to_string(&path)
        .map_err(|e| format!("{}: {e} (queqiao-server relay-key first)", path.display()))?;
    let (name, key) = link::parse_key_line(&line)?;
    let capacity = capacity_mbps(args)?;
    let mut config = turn_config(args, &[])?.ok_or("relay: --turn-port 0 leaves nothing to do")?;
    config.secret = key;
    let turn = Arc::new(config);
    println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
    println!(
        "relay {name}: STUN/TURN on UDP {} for {}, relay ports {}-{}, {capacity} Mbit/s; account server {central}",
        turn.listen, turn.public_ip, turn.relay_ports.0, turn.relay_ports.1
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let side = link::RelaySide {
            centrals,
            name,
            key,
            turn: turn.clone(),
            capacity_mbps: capacity,
            standby,
        };
        tokio::select! {
            result = turn::serve(turn) => result.map_err(|e| format!("STUN/TURN: {e}")),
            () = link::keep_linked(side) => Ok(()),
        }
    })
}

fn serve(args: &[String]) -> Result<(), String> {
    let folder = data_folder(args)?;
    let listen: SocketAddr = values(args, "--listen")
        .pop()
        .unwrap_or_else(|| "0.0.0.0:47100".into())
        .parse()
        .map_err(|e| format!("--listen: {e}"))?;
    let names = values(args, "--name");
    let (tls, fingerprint) = cert::load_or_create(&folder, &names)?;
    let store = Store::open(&folder.join("queqiao.db"))?;
    let turn = turn_config(args, &names)?.map(Arc::new);
    println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
    println!("listening on {listen}, data in {}", folder.display());
    println!("certificate fingerprint (clients: --pin): {fingerprint}");
    match &turn {
        Some(turn) => println!(
            "STUN/TURN on UDP {} for {}, relay ports {}-{}{}",
            turn.listen,
            turn.public_ip,
            turn.relay_ports.0,
            turn.relay_ports.1,
            if turn.allow_private {
                ", private addresses allowed"
            } else {
                ""
            }
        ),
        None => println!("STUN/TURN off"),
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    // New accounts from the programs' login forms: only when the server's owner allows it.
    let signup = args.iter().any(|arg| arg == "--allow-signup");
    println!(
        "new accounts from the login form: {}",
        if signup {
            "allowed (--allow-signup)"
        } else {
            "no (queqiao-server add-user)"
        }
    );
    let capacity = capacity_mbps(args)?;
    // The name the session windows show for this server's own relay ("beijing").
    let relay_name = values(args, "--relay-name").pop().unwrap_or_default();
    if !relay_name.is_empty() && !store::valid_relay_name(&relay_name) {
        return Err("--relay-name: 1 to 32 letters, digits, - or _".into());
    }
    let relays = relays::Relays::new(turn.clone(), capacity, &relay_name);
    let hub = hub::Hub::new(store, relays.clone(), signup);
    runtime.block_on(async move {
        tokio::spawn(relays.clone().watch_local());
        // The change log keeps the last changes only (a standby further behind gets a full copy).
        let pruning = hub.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                pruning.prune_changes();
            }
        });
        // The relays' state in the log every few minutes.
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                println!("relays: {}", relays.summary());
            }
        });
        let accounts = hub.serve(listen, tokio_rustls::TlsAcceptor::from(tls));
        match turn {
            Some(turn) => tokio::select! {
                result = accounts => result.map_err(|e| format!("accounts: {e}")),
                result = turn::serve(turn) => result.map_err(|e| format!("STUN/TURN: {e}")),
            },
            None => accounts.await.map_err(|e| format!("accounts: {e}")),
        }
    })
}

/// STUN/TURN settings from the options; `None` with --turn-port 0.
fn turn_config(args: &[String], names: &[String]) -> Result<Option<turn::TurnConfig>, String> {
    let port: u16 = match values(args, "--turn-port").pop() {
        Some(port) => port.parse().map_err(|e| format!("--turn-port: {e}"))?,
        None => 3478,
    };
    if port == 0 {
        return Ok(None);
    }
    let public_ip: IpAddr = match values(args, "--public-ip").pop() {
        Some(ip) => ip.parse().map_err(|e| format!("--public-ip: {e}"))?,
        None => names
            .iter()
            .find_map(|name| name.parse().ok())
            .ok_or("STUN/TURN needs the address clients reach this server at: --public-ip <IP> (or --turn-port 0)")?,
    };
    let relay_ports = match values(args, "--relay-ports").pop() {
        Some(range) => {
            let (first, last) = range.split_once('-').ok_or("--relay-ports: first-last")?;
            let first: u16 = first.parse().map_err(|e| format!("--relay-ports: {e}"))?;
            let last: u16 = last.parse().map_err(|e| format!("--relay-ports: {e}"))?;
            if first == 0 || last < first {
                return Err("--relay-ports: first-last, first above 0".into());
            }
            (first, last)
        }
        None => (47200, 47399),
    };
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|e| e.to_string())?;
    Ok(Some(turn::TurnConfig::new(
        SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port),
        public_ip,
        relay_ports,
        args.iter().any(|arg| arg == "--relay-private"),
        secret,
        capacity_mbps(args)? * 1e6 / 8.0,
    )))
}
