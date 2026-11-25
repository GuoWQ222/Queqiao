//! What the server keeps (SQLite): accounts with Argon2id password hashes, devices, and refresh tokens
//! (only their SHA-256 is stored, so a copy of the database does not log anyone in).

use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use rusqlite::{Connection, OptionalExtension, params};
use sha2::{Digest, Sha256};

/// Argon2id with OWASP's minimum: 19 MiB, 2 passes, 1 lane.
const MEMORY_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const LANES: u32 = 1;
/// Refresh tokens expire after this many days without a new login.
const TOKEN_DAYS: i64 = 90;

/// A device's owner, name, MAC addresses and the address it last connected from.
pub struct WakeInfo {
    pub user_id: i64,
    pub name: String,
    pub macs: Vec<String>,
    pub address: String,
}

pub struct Store {
    db: Connection,
}

pub struct UserRow {
    pub id: i64,
    pub name: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Store, String> {
        let db = Connection::open(path).map_err(|e| e.to_string())?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS users (
                 id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE, password TEXT NOT NULL,
                 created INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS devices (
                 id TEXT PRIMARY KEY, user_id INTEGER NOT NULL REFERENCES users(id), name TEXT NOT NULL,
                 created INTEGER NOT NULL, last_seen INTEGER NOT NULL, public_key TEXT NOT NULL DEFAULT '');
             CREATE TABLE IF NOT EXISTS refresh_tokens (
                 hash TEXT PRIMARY KEY, device_id TEXT NOT NULL REFERENCES devices(id), role TEXT NOT NULL,
                 user_id INTEGER NOT NULL REFERENCES users(id), expires INTEGER NOT NULL);",
        )
        .map_err(|e| e.to_string())?;
        // User names are one account whatever their upper and lower case ("Alice" is "alice"): the lockout counts by
        // the name in lower case, and two accounts differing only in case could clear each other's count (the
        // security review of 2026-11-22).
        db.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS users_name_nocase ON users(name COLLATE NOCASE)")
            .map_err(|e| {
                format!("{e}: two user names in this database differ only in upper and lower case; rename one first")
            })?;
        // Databases from before device keys (M3) get the column.
        let has_key: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'public_key'")
            .and_then(|mut query| query.exists([]))
            .map_err(|e| e.to_string())?;
        if !has_key {
            db.execute_batch("ALTER TABLE devices ADD COLUMN public_key TEXT NOT NULL DEFAULT ''")
                .map_err(|e| e.to_string())?;
        }
        // Wake-on-LAN: the device's MAC addresses and the address it last connected from.
        for (column, definition) in [
            (
                "macs",
                "ALTER TABLE devices ADD COLUMN macs TEXT NOT NULL DEFAULT ''",
            ),
            (
                "last_address",
                "ALTER TABLE devices ADD COLUMN last_address TEXT NOT NULL DEFAULT ''",
            ),
            // The PC a device runs on (a salted hash of its machine id): a reinstalled PC replaces its earlier device.
            (
                "machine",
                "ALTER TABLE devices ADD COLUMN machine TEXT NOT NULL DEFAULT ''",
            ),
        ] {
            let present: bool = db
                .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = ?1")
                .and_then(|mut query| query.exists([column]))
                .map_err(|e| e.to_string())?;
            if !present {
                db.execute_batch(definition).map_err(|e| e.to_string())?;
            }
        }
        // Help IDs (short numbers for helping someone of another account), unique when set.
        let has_help_id: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'help_id'")
            .and_then(|mut query| query.exists([]))
            .map_err(|e| e.to_string())?;
        if !has_help_id {
            db.execute_batch(
                "ALTER TABLE devices ADD COLUMN help_id TEXT NOT NULL DEFAULT '';
                 CREATE UNIQUE INDEX IF NOT EXISTS devices_help_id ON devices(help_id) WHERE help_id != '';",
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(Store { db })
    }

    pub fn add_user(&self, name: &str, password: &str) -> Result<(), String> {
        if name.trim().is_empty() || password.len() < 8 {
            return Err("a user name and a password of at least 8 characters are needed".into());
        }
        self.add_user_hashed(name, &hash_password(password)?)
    }

    /// A new user with a password hashed already (new_password_hash, without the store's lock).
    pub fn add_user_hashed(&self, name: &str, hash: &str) -> Result<(), String> {
        if self.user_exists(name)? {
            return Err("that user name is taken".into());
        }
        self.db
            .execute(
                "INSERT INTO users (name, password, created) VALUES (?1, ?2, ?3)",
                params![name, hash, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Takes a user out with their devices and logins; false when there is no such user.
    pub fn remove_user(&mut self, name: &str) -> Result<bool, String> {
        let transaction = self.db.transaction().map_err(|e| e.to_string())?;
        let id: Option<i64> = transaction
            .query_row(
                "SELECT id FROM users WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some(id) = id else {
            return Ok(false);
        };
        for statement in [
            "DELETE FROM refresh_tokens WHERE user_id = ?1",
            "DELETE FROM devices WHERE user_id = ?1",
            "DELETE FROM users WHERE id = ?1",
        ] {
            transaction
                .execute(statement, params![id])
                .map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// The account a device belongs to, if it is known.
    pub fn device_owner(&self, device_id: &str) -> Result<Option<i64>, String> {
        self.db
            .query_row(
                "SELECT user_id FROM devices WHERE id = ?1",
                params![device_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// Whether `device_id` may log in to the account `user_id`, asked before the password is checked: not a device of
    /// another account, and not a new one past MAX_DEVICES.
    pub fn device_admissible(
        &self,
        user_id: i64,
        device_id: &str,
        name: &str,
        machine: &str,
    ) -> Result<(), String> {
        match self.device_owner(device_id)? {
            Some(owner) if owner != user_id => Err("this device belongs to another account".into()),
            Some(_) => Ok(()),
            // A reinstalled PC takes its earlier device's place, even in a full account.
            None if !self
                .same_machine(user_id, device_id, name, machine)?
                .is_empty() =>
            {
                Ok(())
            }
            None => {
                let devices: i64 = self
                    .db
                    .query_row(
                        "SELECT COUNT(*) FROM devices WHERE user_id = ?1",
                        params![user_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                if devices >= MAX_DEVICES {
                    return Err(format!(
                        "this account has {MAX_DEVICES} devices, the most it may have: remove one first"
                    ));
                }
                Ok(())
            }
        }
    }

    pub fn user_exists(&self, name: &str) -> Result<bool, String> {
        self.db
            .prepare("SELECT 1 FROM users WHERE name = ?1 COLLATE NOCASE")
            .and_then(|mut query| query.exists(params![name]))
            .map_err(|e| e.to_string())
    }

    /// The user if the password is right.
    #[cfg(test)]
    pub fn check_password(&self, name: &str, password: &str) -> Result<Option<UserRow>, String> {
        Ok(self.password_record(name)?.check(password))
    }

    /// A user's stored password, to be checked without the store's lock (PasswordRecord::check).
    pub fn password_record(&self, name: &str) -> Result<PasswordRecord, String> {
        let row: Option<(i64, String, String)> = self
            .db
            .query_row(
                "SELECT id, name, password FROM users WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let (id, name, hash) = row.unwrap_or((-1, String::new(), String::new()));
        Ok(PasswordRecord { id, name, hash })
    }

    /// Records the device and its public key for the user; true if the device is new to the account. A
    /// device id that belongs to another account is refused; a different key for a known device only when
    /// `may_change_key` (a password login).
    pub fn register_device(
        &self,
        user_id: i64,
        device_id: &str,
        name: &str,
        public_key: &str,
        may_change_key: bool,
    ) -> Result<bool, String> {
        if public_key.len() != 64 || !public_key.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("bad device key".into());
        }
        let stored: Option<String> = self
            .db
            .query_row(
                "SELECT public_key FROM devices WHERE id = ?1 AND user_id = ?2",
                params![device_id, user_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // A row without a key (a database from before device keys) gets one only with the password: a saved login
        // alone could otherwise set any key (the security review of 2026-11-22).
        if let Some(stored) = stored
            && !may_change_key
        {
            if stored.is_empty() {
                return Err(
                    "the server has no key for this device yet: log in with the password".into(),
                );
            }
            if !stored.eq_ignore_ascii_case(public_key) {
                return Err("this device has a new key: log in with the password".into());
            }
        }
        let owner: Option<i64> = self
            .db
            .query_row(
                "SELECT user_id FROM devices WHERE id = ?1",
                params![device_id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        match owner {
            Some(owner) if owner != user_id => Err("this device belongs to another account".into()),
            Some(_) => {
                self.db
                    .execute(
                        "UPDATE devices SET name = ?2, last_seen = ?3, public_key = ?4 WHERE id = ?1",
                        params![device_id, name, now(), public_key.to_ascii_lowercase()],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(false)
            }
            None => {
                // MAX_DEVICES an account at most (the security review of 2026-11-22: new device ids without end got
                // around the relay's quota per device, and the device list sent on every connect grew with them).
                let devices: i64 = self
                    .db
                    .query_row(
                        "SELECT COUNT(*) FROM devices WHERE user_id = ?1",
                        params![user_id],
                        |r| r.get(0),
                    )
                    .map_err(|e| e.to_string())?;
                if devices >= MAX_DEVICES {
                    return Err(format!(
                        "this account has {MAX_DEVICES} devices, the most it may have: remove one first"
                    ));
                }
                self.db
                    .execute(
                        "INSERT INTO devices (id, user_id, name, created, last_seen, public_key) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                        params![device_id, user_id, name, now(), public_key.to_ascii_lowercase()],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(true)
            }
        }
    }

    /// A new refresh token for one program (agent or viewer) of the device; its earlier token stops working.
    pub fn issue_refresh_token(
        &self,
        user_id: i64,
        device_id: &str,
        role: &str,
    ) -> Result<String, String> {
        self.db
            .execute(
                "DELETE FROM refresh_tokens WHERE device_id = ?1 AND role = ?2",
                params![device_id, role],
            )
            .map_err(|e| e.to_string())?;
        let token = random_hex(32)?;
        let expires = now() + TOKEN_DAYS * 86_400;
        self.db
            .execute(
                "INSERT INTO refresh_tokens (hash, device_id, role, user_id, expires) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![sha256_hex(token.as_bytes()), device_id, role, user_id, expires],
            )
            .map_err(|e| e.to_string())?;
        Ok(token)
    }

    /// The user a refresh token belongs to, if it is valid for this device and program.
    pub fn check_refresh_token(
        &self,
        token: &str,
        device_id: &str,
        role: &str,
    ) -> Result<Option<UserRow>, String> {
        self.db
            .query_row(
                "SELECT u.id, u.name FROM refresh_tokens t JOIN users u ON u.id = t.user_id
                 WHERE t.hash = ?1 AND t.device_id = ?2 AND t.role = ?3 AND t.expires > ?4",
                params![sha256_hex(token.as_bytes()), device_id, role, now()],
                |r| {
                    Ok(UserRow {
                        id: r.get(0)?,
                        name: r.get(1)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    /// The user's other devices on the same PC under the same name: what a password login of `device_id` replaces
    /// (the PC was reinstalled and made a new device id; its earlier device stayed in the list, offline, 2026-11-26).
    /// Nothing for an empty tag (programs from before it). The name too, so two PCs cloned from one image (one
    /// machine id) and named apart never replace each other.
    pub fn same_machine(
        &self,
        user_id: i64,
        device_id: &str,
        name: &str,
        machine: &str,
    ) -> Result<Vec<String>, String> {
        if machine.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = self
            .db
            .prepare(
                "SELECT id FROM devices WHERE user_id = ?1 AND id != ?2 AND name = ?3 AND machine = ?4 ORDER BY id",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map(
                params![user_id, device_id, name, machine.to_ascii_lowercase()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())
    }

    /// Records the PC a device runs on (see [`Store::same_machine`]).
    pub fn set_machine(&self, device_id: &str, machine: &str) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE devices SET machine = ?2 WHERE id = ?1",
                params![device_id, machine.to_ascii_lowercase()],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Removes a device of the user with its refresh tokens; false if the user has no such device.
    pub fn remove_device(&self, user_id: i64, device_id: &str) -> Result<bool, String> {
        let owned: bool = self
            .db
            .query_row(
                "SELECT 1 FROM devices WHERE id = ?1 AND user_id = ?2",
                params![device_id, user_id],
                |_| Ok(true),
            )
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        if !owned {
            return Ok(false);
        }
        self.db
            .execute(
                "DELETE FROM refresh_tokens WHERE device_id = ?1",
                params![device_id],
            )
            .map_err(|e| e.to_string())?;
        self.db
            .execute("DELETE FROM devices WHERE id = ?1", params![device_id])
            .map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Sets a new password after checking the old one, and revokes every refresh token of the account.
    #[cfg(test)]
    pub fn change_password(
        &self,
        user_id: i64,
        name: &str,
        old: &str,
        new: &str,
    ) -> Result<(), String> {
        if new.len() < 8 {
            return Err("the new password needs at least 8 characters".into());
        }
        if self
            .check_password(name, old)?
            .is_none_or(|user| user.id != user_id)
        {
            return Err("the current password is wrong".into());
        }
        self.set_password_hash(user_id, &hash_password(new)?)
    }

    /// Sets a password hashed already (new_password_hash) and revokes every refresh token of the account.
    pub fn set_password_hash(&self, user_id: i64, hash: &str) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE users SET password = ?2 WHERE id = ?1",
                params![user_id, hash],
            )
            .map_err(|e| e.to_string())?;
        self.db
            .execute(
                "DELETE FROM refresh_tokens WHERE user_id = ?1",
                params![user_id],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Logs one program of a device out: its refresh token stops working.
    pub fn revoke_token(&self, device_id: &str, role: &str) -> Result<(), String> {
        self.db
            .execute(
                "DELETE FROM refresh_tokens WHERE device_id = ?1 AND role = ?2",
                params![device_id, role],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The account's devices: id, name and public key, oldest first.
    /// The device's help ID: a 9-digit number, made when first asked for and kept.
    pub fn help_id(&self, device_id: &str) -> Result<String, String> {
        let existing: String = self
            .db
            .query_row(
                "SELECT help_id FROM devices WHERE id = ?1",
                [device_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        if !existing.is_empty() {
            return Ok(existing);
        }
        for _ in 0..50 {
            let mut bytes = [0u8; 8];
            getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
            let candidate = (100_000_000 + u64::from_le_bytes(bytes) % 900_000_000).to_string();
            let taken = self
                .db
                .prepare("SELECT 1 FROM devices WHERE help_id = ?1")
                .and_then(|mut query| query.exists([&candidate]))
                .map_err(|e| e.to_string())?;
            if !taken {
                self.db
                    .execute(
                        "UPDATE devices SET help_id = ?1 WHERE id = ?2",
                        [&candidate, device_id],
                    )
                    .map_err(|e| e.to_string())?;
                return Ok(candidate);
            }
        }
        Err("no free help ID".into())
    }

    /// A new help ID for the device: the old one is given up (it no longer finds the device) and a new one made.
    pub fn new_help_id(&self, device_id: &str) -> Result<String, String> {
        self.db
            .execute("UPDATE devices SET help_id = '' WHERE id = ?1", [device_id])
            .map_err(|e| e.to_string())?;
        self.help_id(device_id)
    }

    /// (device id, user id, name, public key) of the device with this help ID.
    pub fn device_by_help_id(
        &self,
        help_id: &str,
    ) -> Result<Option<(String, i64, String, String)>, String> {
        if help_id.is_empty() {
            return Ok(None);
        }
        self.db
            .query_row(
                "SELECT id, user_id, name, public_key FROM devices WHERE help_id = ?1",
                [help_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn set_macs(&self, device_id: &str, macs: &[String]) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE devices SET macs = ?1 WHERE id = ?2",
                params![macs.join(","), device_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    pub fn set_last_address(&self, device_id: &str, address: &str) -> Result<(), String> {
        self.db
            .execute(
                "UPDATE devices SET last_address = ?1 WHERE id = ?2",
                params![address, device_id],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// What waking a device needs to know about it.
    pub fn wake_info(&self, device_id: &str) -> Result<Option<WakeInfo>, String> {
        self.db
            .query_row(
                "SELECT user_id, name, macs, last_address FROM devices WHERE id = ?1",
                [device_id],
                |row| {
                    let macs: String = row.get(2)?;
                    Ok(WakeInfo {
                        user_id: row.get(0)?,
                        name: row.get(1)?,
                        macs: macs
                            .split(',')
                            .filter(|m| !m.is_empty())
                            .map(str::to_string)
                            .collect(),
                        address: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(|e| e.to_string())
    }

    pub fn public_key(&self, device_id: &str) -> Result<String, String> {
        self.db
            .query_row(
                "SELECT public_key FROM devices WHERE id = ?1",
                [device_id],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
    }

    pub fn devices(&self, user_id: i64) -> Result<Vec<(String, String, String)>, String> {
        let mut query = self
            .db
            .prepare("SELECT id, name, public_key FROM devices WHERE user_id = ?1 ORDER BY created")
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map(params![user_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }
}

/// "argon2id$m=..,t=..,p=..$<salt hex>$<hash hex>" (the parameters are kept, so they can change later).
/// Devices an account may have at most.
pub const MAX_DEVICES: i64 = 64;

/// A user's stored password (its Argon2 hash), read under the store's lock and checked without it: hashing takes
/// tens of milliseconds, and every other connection waited for the lock meanwhile (the security review of 2026-11-22).
/// An unknown user's never matches (and still costs one hash, so the answer takes the same time).
pub struct PasswordRecord {
    /// -1 for a name no account has (its check always fails).
    pub id: i64,
    name: String,
    hash: String,
}

impl PasswordRecord {
    pub fn check(&self, password: &str) -> Option<UserRow> {
        (verify_password(password, &self.hash) && self.id >= 0).then(|| UserRow {
            id: self.id,
            name: self.name.clone(),
        })
    }
}

/// A new password's hash, made without the store's lock (add_user_hashed, set_password_hash).
pub fn new_password_hash(password: &str) -> Result<String, String> {
    hash_password(password)
}

fn hash_password(password: &str) -> Result<String, String> {
    let salt = random_bytes(16)?;
    let hash = argon2(password, &salt, MEMORY_KIB, PASSES, LANES)?;
    Ok(format!(
        "argon2id$m={MEMORY_KIB},t={PASSES},p={LANES}${}${}",
        hex(&salt),
        hex(&hash)
    ))
}

fn verify_password(password: &str, stored: &str) -> bool {
    let parts: Vec<&str> = stored.split('$').collect();
    let [_, settings, salt, expected] = parts[..] else {
        // Unknown user: still do the work of one hash.
        let _ = argon2(password, &[0; 16], MEMORY_KIB, PASSES, LANES);
        return false;
    };
    let value = |key: &str| {
        settings
            .split(',')
            .find_map(|kv| kv.strip_prefix(key).and_then(|v| v.parse::<u32>().ok()))
    };
    let (Some(m), Some(t), Some(p), Some(salt)) =
        (value("m="), value("t="), value("p="), unhex(salt))
    else {
        return false;
    };
    match argon2(password, &salt, m, t, p) {
        Ok(hash) => constant_time_eq(hex(&hash).as_bytes(), expected.as_bytes()),
        Err(_) => false,
    }
}

fn argon2(
    password: &str,
    salt: &[u8],
    memory: u32,
    passes: u32,
    lanes: u32,
) -> Result<Vec<u8>, String> {
    let params = Params::new(memory, passes, lanes, Some(32)).map_err(|e| e.to_string())?;
    let mut out = vec![0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| e.to_string())?;
    Ok(out)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn random_bytes(count: usize) -> Result<Vec<u8>, String> {
    let mut bytes = vec![0u8; count];
    getrandom::fill(&mut bytes).map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub fn random_hex(count: usize) -> Result<String, String> {
    Ok(hex(&random_bytes(count)?))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex(Sha256::digest(data).as_slice())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_names_are_one_whatever_their_case() {
        let dir = std::env::temp_dir().join(format!("queqiao-store-case-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store.add_user("alice", "correct horse").unwrap();
        assert!(store.add_user("Alice", "another horse").is_err());
        assert!(store.add_user("ALICE", "another horse").is_err());
        assert!(store.user_exists("ALICE").unwrap());
        let row = store
            .check_password("ALICE", "correct horse")
            .unwrap()
            .unwrap();
        assert_eq!(row.name, "alice");
        assert!(
            store
                .check_password("Alice", "wrong horse")
                .unwrap()
                .is_none()
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn passwords_and_tokens() {
        let dir = std::env::temp_dir().join(format!("queqiao-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store.add_user("alice", "correct horse").unwrap();
        assert!(store.add_user("alice", "another one").is_err());
        assert!(store.add_user("bob", "short").is_err());
        assert!(
            store
                .check_password("alice", "wrong horse")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_password("nobody", "correct horse")
                .unwrap()
                .is_none()
        );
        let user = store
            .check_password("alice", "correct horse")
            .unwrap()
            .unwrap();
        let key_a = "a".repeat(64);
        let key_b = "b".repeat(64);
        store
            .register_device(user.id, "dev1", "Laptop", &key_a, false)
            .unwrap();
        assert!(
            store
                .register_device(user.id, "dev1", "Laptop", &key_b, false)
                .is_err(),
            "resume cannot change the key"
        );
        store
            .register_device(user.id, "dev1", "Laptop", &key_b, true)
            .unwrap();
        store
            .register_device(user.id, "dev1", "Laptop", &key_b, false)
            .unwrap();
        assert!(
            store
                .register_device(user.id, "dev2", "Laptop", "not hex", true)
                .is_err()
        );
        let token = store.issue_refresh_token(user.id, "dev1", "agent").unwrap();
        assert_eq!(
            store
                .check_refresh_token(&token, "dev1", "agent")
                .unwrap()
                .unwrap()
                .name,
            "alice"
        );
        assert!(
            store
                .check_refresh_token(&token, "dev2", "agent")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_refresh_token(&token, "dev1", "viewer")
                .unwrap()
                .is_none()
        );
        let viewer = store
            .issue_refresh_token(user.id, "dev1", "viewer")
            .unwrap();
        let second = store.issue_refresh_token(user.id, "dev1", "agent").unwrap();
        assert!(
            store
                .check_refresh_token(&token, "dev1", "agent")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_refresh_token(&second, "dev1", "agent")
                .unwrap()
                .is_some()
        );
        assert!(
            store
                .check_refresh_token(&viewer, "dev1", "viewer")
                .unwrap()
                .is_some()
        );
        store.revoke_token("dev1", "viewer").unwrap();
        assert!(
            store
                .check_refresh_token(&viewer, "dev1", "viewer")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_refresh_token(&second, "dev1", "agent")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            store.devices(user.id).unwrap(),
            vec![("dev1".into(), "Laptop".into(), key_b.clone())]
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removing_devices_and_changing_passwords() {
        let dir = std::env::temp_dir().join(format!("queqiao-store-test2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store.add_user("carol", "correct horse").unwrap();
        let user = store
            .check_password("carol", "correct horse")
            .unwrap()
            .unwrap();
        let key = "c".repeat(64);
        assert!(
            store
                .register_device(user.id, "dev1", "PC", &key, true)
                .unwrap(),
            "new device"
        );
        assert!(
            !store
                .register_device(user.id, "dev1", "PC", &key, true)
                .unwrap(),
            "known device"
        );
        let token = store
            .issue_refresh_token(user.id, "dev1", "viewer")
            .unwrap();
        // A password change needs the current password and revokes every token of the account.
        assert!(
            store
                .change_password(user.id, "carol", "wrong horse", "new horse staple")
                .is_err()
        );
        assert!(
            store
                .change_password(user.id, "carol", "correct horse", "short")
                .is_err()
        );
        store
            .change_password(user.id, "carol", "correct horse", "new horse staple")
            .unwrap();
        assert!(
            store
                .check_refresh_token(&token, "dev1", "viewer")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_password("carol", "correct horse")
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .check_password("carol", "new horse staple")
                .unwrap()
                .is_some()
        );
        // Only the owner removes a device; its tokens go with it.
        let token = store
            .issue_refresh_token(user.id, "dev1", "viewer")
            .unwrap();
        assert!(!store.remove_device(user.id + 1, "dev1").unwrap());
        assert!(store.remove_device(user.id, "dev1").unwrap());
        assert!(
            store
                .check_refresh_token(&token, "dev1", "viewer")
                .unwrap()
                .is_none()
        );
        assert!(store.devices(user.id).unwrap().is_empty());
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_account_has_at_most_max_devices_and_hashes_outside_the_lock() {
        let dir = std::env::temp_dir().join(format!("queqiao-store-max-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        // Made and checked the way the server does: the hash made, the record read, each without the lock.
        store
            .add_user_hashed("dora", &new_password_hash("correct horse").unwrap())
            .unwrap();
        let record = store.password_record("dora").unwrap();
        let user = record.check("correct horse").unwrap();
        assert!(record.check("wrong horse").is_none());
        assert!(store.password_record("nobody").unwrap().check("").is_none());
        for i in 0..MAX_DEVICES {
            let key = format!("{i:064x}");
            assert!(
                store
                    .register_device(user.id, &format!("d{i}"), "PC", &key, true)
                    .unwrap()
            );
        }
        let refused =
            store.register_device(user.id, "one-more", "PC", &format!("{:064x}", 999), true);
        assert!(refused.is_err_and(|e| e.contains("most it may have")));
        // Asked before the password: a new device past the most, another account's device.
        assert!(
            store
                .device_admissible(user.id, "one-more", "PC", "")
                .is_err()
        );
        assert!(store.device_admissible(user.id, "d0", "PC", "").is_ok());
        assert!(
            store
                .device_admissible(user.id + 1, "d0", "PC", "")
                .is_err()
        );
        assert!(store.device_admissible(-1, "brand-new", "PC", "").is_ok());
        // A known device still logs in.
        assert!(
            !store
                .register_device(user.id, "d0", "PC", &format!("{:064x}", 0), true)
                .unwrap()
        );
        // A reinstalled PC (a new device id, the same PC and name) takes its earlier device's place, even in a full
        // account; a PC of the same image under another name, or a program without the tag, does not.
        let machine = "ab".repeat(32);
        store.set_machine("d3", &machine).unwrap();
        assert!(
            store
                .device_admissible(user.id, "d3-again", "PC", &machine)
                .is_ok()
        );
        assert_eq!(
            store
                .same_machine(user.id, "d3-again", "PC", &machine.to_ascii_uppercase())
                .unwrap(),
            vec!["d3".to_string()]
        );
        assert!(
            store
                .same_machine(user.id, "d3-again", "Other PC", &machine)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .same_machine(user.id, "d3-again", "PC", "")
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .same_machine(user.id + 1, "d3-again", "PC", &machine)
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .device_admissible(user.id, "d3-again", "Other PC", &machine)
                .is_err()
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_device_without_a_key_gets_one_only_with_the_password() {
        let dir =
            std::env::temp_dir().join(format!("queqiao-store-keyless-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        let _ = std::fs::remove_file(&path);
        let store = Store::open(&path).unwrap();
        store.add_user("erin", "correct horse").unwrap();
        let user = store
            .check_password("erin", "correct horse")
            .unwrap()
            .unwrap();
        // As a database from before device keys has it.
        store
            .db
            .execute(
                "INSERT INTO devices (id, user_id, name, created, last_seen) VALUES ('old-device', ?1, 'PC', 0, 0)",
                params![user.id],
            )
            .unwrap();
        let key = format!("{:064x}", 7);
        let resumed = store.register_device(user.id, "old-device", "PC", &key, false);
        assert!(resumed.is_err_and(|e| e.contains("log in with the password")));
        assert!(
            !store
                .register_device(user.id, "old-device", "PC", &key, true)
                .unwrap()
        );
        assert_eq!(store.public_key("old-device").unwrap(), key);
        assert!(
            !store
                .register_device(user.id, "old-device", "PC", &key, false)
                .unwrap()
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
