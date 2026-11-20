//! queqiao-server: accounts, devices, presence, and the signaling that lets an account's devices connect
//! to each other (TLS WebSockets). The STUN/TURN relay follows in M4.
//!
//! queqiao-server serve [--listen 0.0.0.0:47100] [--data <folder>] [--name <host name or IP>]...
//!     Starts the server. On first start it makes a self-signed certificate for the given names and prints
//!     its SHA-256 fingerprint, which clients pin (--pin).
//! queqiao-server init [--data <folder>] [--name <host name or IP>]...
//!     Makes the certificate if there is none yet and prints its fingerprint.
//! queqiao-server add-user <name> <password> [--data <folder>]
//! Without arguments it prints its version.

mod cert;
mod hub;
mod store;

use std::net::SocketAddr;
use std::path::PathBuf;

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
    let (tls, fingerprint) = cert::load_or_create(&folder, &values(args, "--name"))?;
    let store = Store::open(&folder.join("queqiao.db"))?;
    println!("{}", queqiao_core::banner(env!("CARGO_PKG_NAME")));
    println!("listening on {listen}, data in {}", folder.display());
    println!("certificate fingerprint (clients: --pin): {fingerprint}");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let hub = hub::Hub::new(store);
    runtime
        .block_on(hub.serve(listen, tokio_rustls::TlsAcceptor::from(tls)))
        .map_err(|e| e.to_string())
}
