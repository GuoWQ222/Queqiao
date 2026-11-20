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
//! queqiao-server add-user <name> <password> [--data <folder>]
//! Without arguments it prints its version.

mod cert;
mod hub;
mod store;
mod turn;

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use store::Store;

fn main() {
    let banner = queqiao_core::banner(env!("CARGO_PKG_NAME"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("add-user") => add_user(&args[1..]),
        Some("init") => init(&args[1..]),
        _ => {
            println!("{banner}");
            println!(
                "usage: queqiao-server serve [--listen 0.0.0.0:47100] [--data <folder>] [--name <host or IP>]..."
            );
            println!(
                "                            [--public-ip <IP>] [--turn-port 3478] [--relay-ports 47200-47399] [--relay-private]"
            );
            println!("       queqiao-server init [--data <folder>] [--name <host or IP>]...");
            println!("       queqiao-server add-user <name> <password> [--data <folder>]");
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
    Ok(folder)
}

fn add_user(args: &[String]) -> Result<(), String> {
    let (Some(name), Some(password)) = (args.first(), args.get(1)) else {
        return Err("usage: queqiao-server add-user <name> <password> [--data <folder>]".into());
    };
    let store = Store::open(&data_folder(args)?.join("queqiao.db"))?;
    store.add_user(name, password)?;
    println!("user {name} added");
    Ok(())
}

fn init(args: &[String]) -> Result<(), String> {
    let (_, fingerprint) = cert::load_or_create(&data_folder(args)?, &values(args, "--name"))?;
    println!("certificate fingerprint: {fingerprint}");
    Ok(())
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
    let hub = hub::Hub::new(store, turn.clone());
    runtime.block_on(async move {
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
    Ok(Some(turn::TurnConfig {
        listen: SocketAddr::new(IpAddr::from([0, 0, 0, 0]), port),
        public_ip,
        relay_ports,
        allow_private: args.iter().any(|arg| arg == "--relay-private"),
        secret,
    }))
}
