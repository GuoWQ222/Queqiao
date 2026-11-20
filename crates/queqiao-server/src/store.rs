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
        // Databases from before device keys (M3) get the column.
        let has_key: bool = db
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'public_key'")
            .and_then(|mut query| query.exists([]))
            .map_err(|e| e.to_string())?;
        if !has_key {
            db.execute_batch("ALTER TABLE devices ADD COLUMN public_key TEXT NOT NULL DEFAULT ''")
                .map_err(|e| e.to_string())?;
        }
        Ok(Store { db })
    }

    pub fn add_user(&self, name: &str, password: &str) -> Result<(), String> {
        if name.trim().is_empty() || password.len() < 8 {
            return Err("a user name and a password of at least 8 characters are needed".into());
        }
        let hash = hash_password(password)?;
        self.db
            .execute(
                "INSERT INTO users (name, password, created) VALUES (?1, ?2, ?3)",
                params![name, hash, now()],
            )
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// The user if the password is right.
    pub fn check_password(&self, name: &str, password: &str) -> Result<Option<UserRow>, String> {
        let row: Option<(i64, String)> = self
            .db
            .query_row(
                "SELECT id, password FROM users WHERE name = ?1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        // Hash even for an unknown user, so the answer takes the same time.
        let (id, stored) = row.unwrap_or((-1, String::new()));
        let ok = verify_password(password, &stored);
        Ok((ok && id >= 0).then(|| UserRow {
            id,
            name: name.to_string(),
        }))
    }

    /// Records the device and its public key for the user. A device id that belongs to another account is
    /// refused; a different key for a known device only when `may_change_key` (a password login).
    pub fn register_device(
        &self,
        user_id: i64,
        device_id: &str,
        name: &str,
        public_key: &str,
        may_change_key: bool,
    ) -> Result<(), String> {
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
        if let Some(stored) = stored
            && !stored.is_empty()
            && !stored.eq_ignore_ascii_case(public_key)
            && !may_change_key
        {
            return Err("this device has a new key: log in with the password".into());
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
                Ok(())
            }
            None => {
                self.db
                    .execute(
                        "INSERT INTO devices (id, user_id, name, created, last_seen, public_key) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
                        params![device_id, user_id, name, now(), public_key.to_ascii_lowercase()],
                    )
                    .map_err(|e| e.to_string())?;
                Ok(())
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
}
