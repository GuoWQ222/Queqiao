//! The standby account servers an account server named in its welcome (the user, 2026-11-26: a standby keeps a
//! live copy of the accounts), kept beside the logins so that a program can still connect when its account server
//! cannot be reached: once the standby has been made the account server, the saved login works there as before. Not
//! secret (addresses and certificate fingerprints); the account server, whose certificate is pinned, vouched for them.

use std::collections::BTreeMap;

use queqiao_core::account::Standby;
use queqiao_transport::Error;
use serde::{Deserialize, Serialize};

/// At most this many per account server (a longer list is cut).
const MAX: usize = 8;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    /// By "server pin" of the account server that named them.
    by_server: BTreeMap<String, Vec<Standby>>,
}

fn path() -> Result<std::path::PathBuf, Error> {
    let name = if cfg!(feature = "test-files") {
        "standbys_test.json"
    } else {
        "standbys.json"
    };
    Ok(crate::config::folder()?.join(name))
}

fn key(server: &str, pin: &str) -> String {
    format!("{server} {}", crate::tls::normalize_pin(pin))
}

fn read() -> Saved {
    path()
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The standbys `server` (with certificate `pin`) named last.
pub(crate) fn load(server: &str, pin: &str) -> Vec<Standby> {
    read()
        .by_server
        .remove(&key(server, pin))
        .unwrap_or_default()
}

/// What `server` named now (`Welcome`), kept when it changed. Itself and anything malformed are left out.
pub(crate) fn save(server: &str, pin: &str, named: &[Standby]) -> Result<(), Error> {
    let own = crate::tls::normalize_pin(pin);
    let mut standbys: Vec<Standby> = named
        .iter()
        .filter(|standby| {
            let theirs = crate::tls::normalize_pin(&standby.pin);
            theirs.len() == 64 && theirs != own && !standby.server.trim().is_empty()
        })
        .take(MAX)
        .map(|standby| Standby {
            server: standby.server.trim().to_string(),
            pin: crate::tls::normalize_pin(&standby.pin),
        })
        .collect();
    standbys.dedup();
    let mut saved = read();
    let key = key(server, pin);
    if saved
        .by_server
        .get(&key)
        .map_or(standbys.is_empty(), |old| *old == standbys)
    {
        return Ok(());
    }
    if standbys.is_empty() {
        saved.by_server.remove(&key);
    } else {
        saved.by_server.insert(key, standbys);
    }
    let text = serde_json::to_string_pretty(&saved).map_err(|e| Error(e.to_string()))?;
    std::fs::write(path()?, text).map_err(|e| Error(e.to_string()))
}
