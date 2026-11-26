//! A standby account server's live copy of the accounts (the user, 2026-11-26: "How would you sync the accounts?").
//!
//! One server writes: the account server. Every change to the accounts' tables lands in a numbered change log in the
//! same transaction (triggers, so the command-line tools' changes are in it too, and a column added later is copied
//! without more code: the triggers are made from each table's columns when the database opens). A standby (a relay
//! server started with `--standby`) asks over its link for the changes after the last one it applied, or for a full
//! copy when it has none or is further behind than the log keeps, and applies them as they come.
//!
//! The copy holds what the account server holds, which by itself logs nobody in: Argon2id password hashes, the
//! SHA-256 of refresh tokens, devices' public keys, relays' keys. Promoted (`queqiao-server serve` on its folder), the
//! standby takes over with everyone's saved logins still valid.

use rusqlite::types::Value as SqlValue;
use rusqlite::{OptionalExtension, params_from_iter};
use serde::{Deserialize, Serialize};

use crate::store::Store;

/// The tables copied, each with its key column.
pub const TABLES: [(&str, &str); 4] = [
    ("users", "id"),
    ("devices", "id"),
    ("refresh_tokens", "hash"),
    ("relays", "name"),
];
/// The change log keeps this many changes; a standby further behind gets a full copy.
const KEEP_CHANGES: i64 = 200_000;
/// Changes (or rows of a full copy) per message.
pub const BATCH: usize = 200;

/// One change: the row as it now is (a JSON object of its columns), or its removal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub seq: i64,
    pub table: String,
    /// "upsert" or "delete"; "mark" is the first entry of an empty log (so a full copy is always up to a change of
    /// 1 or more) and changes nothing.
    pub op: String,
    pub key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
}

/// What a standby is sent next.
pub enum Batch {
    /// Changes after the one asked from (empty: none yet).
    Changes(Vec<Change>),
    /// The standby is too far behind (or has nothing): a full copy, every row as an upsert, up to change `upto`.
    Copy { upto: i64, rows: Vec<Change> },
}

fn columns(db: &rusqlite::Connection, table: &str) -> Result<Vec<String>, String> {
    let mut query = db
        .prepare("SELECT name FROM pragma_table_info(?1) ORDER BY cid")
        .map_err(|e| e.to_string())?;
    let names = query
        .query_map([table], |row| row.get::<_, String>(0))
        .map_err(|e| e.to_string())?;
    names.collect::<Result<_, _>>().map_err(|e| e.to_string())
}

/// json_object(...) of `prefix`'s columns, in SQL. The names come from the table's own schema.
fn json_of(columns: &[String], prefix: &str) -> String {
    let pairs: Vec<String> = columns
        .iter()
        .map(|column| format!("'{column}', {prefix}.\"{column}\""))
        .collect();
    format!("json_object({})", pairs.join(", "))
}

fn sql_value(value: &serde_json::Value) -> SqlValue {
    match value {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::Bool(b) => SqlValue::Integer(i64::from(*b)),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(0.0)),
        },
        serde_json::Value::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

impl Store {
    /// The change log and its triggers (made again when a table's columns changed).
    pub(crate) fn install_change_log(&self) -> Result<(), String> {
        let db = &self.db;
        db.execute_batch(
            "CREATE TABLE IF NOT EXISTS changes (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT, tbl TEXT NOT NULL, op TEXT NOT NULL, key TEXT NOT NULL,
                 row TEXT);
             CREATE TABLE IF NOT EXISTS replica_state (id INTEGER PRIMARY KEY CHECK (id = 1), seq INTEGER NOT NULL);
             INSERT INTO changes (tbl, op, key, row) SELECT '', 'mark', '', NULL WHERE NOT EXISTS (SELECT 1 FROM changes);",
        )
        .map_err(|e| e.to_string())?;
        for (table, key) in TABLES {
            let columns = columns(db, table)?;
            let new = json_of(&columns, "NEW");
            let wanted = [
                (
                    format!("changes_{table}_insert"),
                    format!(
                        "CREATE TRIGGER changes_{table}_insert AFTER INSERT ON {table} BEGIN INSERT INTO changes (tbl, op, key, row) VALUES ('{table}', 'upsert', NEW.\"{key}\", {new}); END"
                    ),
                ),
                (
                    format!("changes_{table}_update"),
                    format!(
                        "CREATE TRIGGER changes_{table}_update AFTER UPDATE ON {table} BEGIN INSERT INTO changes (tbl, op, key, row) VALUES ('{table}', 'upsert', NEW.\"{key}\", {new}); END"
                    ),
                ),
                (
                    format!("changes_{table}_delete"),
                    format!(
                        "CREATE TRIGGER changes_{table}_delete AFTER DELETE ON {table} BEGIN INSERT INTO changes (tbl, op, key, row) VALUES ('{table}', 'delete', OLD.\"{key}\", NULL); END"
                    ),
                ),
            ];
            for (name, sql) in wanted {
                let existing: Option<String> = db
                    .query_row(
                        "SELECT sql FROM sqlite_master WHERE type = 'trigger' AND name = ?1",
                        [&name],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(|e| e.to_string())?;
                if existing.as_deref() != Some(sql.as_str()) {
                    db.execute_batch(&format!("DROP TRIGGER IF EXISTS {name}; {sql};"))
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        Ok(())
    }

    /// What a standby that has applied everything up to change `after` gets next: up to BATCH changes, or a full copy
    /// when it has nothing (`after` 0) or the log no longer goes back that far.
    pub fn next_for_standby(&self, after: i64) -> Result<Batch, String> {
        let oldest: Option<i64> = self
            .db
            .query_row("SELECT MIN(seq) FROM changes", [], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        if after <= 0 || oldest.is_some_and(|oldest| after + 1 < oldest) {
            return self.full_copy();
        }
        let mut query = self
            .db
            .prepare(
                "SELECT seq, tbl, op, key, row FROM changes WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .map_err(|e| e.to_string())?;
        let rows = query
            .query_map(rusqlite::params![after, BATCH as i64], |row| {
                Ok(Change {
                    seq: row.get(0)?,
                    table: row.get(1)?,
                    op: row.get(2)?,
                    key: row.get(3)?,
                    row: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?;
        Ok(Batch::Changes(
            rows.collect::<Result<_, _>>().map_err(|e| e.to_string())?,
        ))
    }

    /// Every row of the copied tables as an upsert, with the change they are up to (read in one transaction).
    fn full_copy(&self) -> Result<Batch, String> {
        let transaction = self.db.unchecked_transaction().map_err(|e| e.to_string())?;
        let upto: i64 = transaction
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM changes", [], |row| {
                row.get(0)
            })
            .map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for (table, key) in TABLES {
            let columns = columns(&transaction, table)?;
            let mut query = transaction
                .prepare(&format!(
                    "SELECT CAST(\"{key}\" AS TEXT), {} FROM {table} AS t",
                    json_of(&columns, "t")
                ))
                .map_err(|e| e.to_string())?;
            let found = query
                .query_map([], |row| {
                    Ok(Change {
                        seq: upto,
                        table: table.to_string(),
                        op: "upsert".into(),
                        key: row.get(0)?,
                        row: Some(row.get(1)?),
                    })
                })
                .map_err(|e| e.to_string())?;
            for change in found {
                rows.push(change.map_err(|e| e.to_string())?);
            }
        }
        Ok(Batch::Copy { upto, rows })
    }

    /// Keeps the last KEEP_CHANGES changes.
    pub fn prune_changes(&self) -> Result<(), String> {
        self.db
            .execute(
                "DELETE FROM changes WHERE seq <= (SELECT MAX(seq) FROM changes) - ?1",
                [KEEP_CHANGES],
            )
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// A standby: the account server's change it has applied up to (0: none, a full copy is needed).
    pub fn replica_seq(&self) -> Result<i64, String> {
        self.db
            .query_row("SELECT seq FROM replica_state WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()
            .map(|seq| seq.unwrap_or(0))
            .map_err(|e| e.to_string())
    }

    /// A standby applies what the account server sent, in one transaction: `reset` first empties the copied tables
    /// (the start of a full copy); `upto` is where it then stands (0: in the middle of a full copy, so a link lost now
    /// starts the copy again). Foreign keys are not checked meanwhile: the copy is what the account server holds once
    /// a batch is in, not row by row (emptying `users` before `devices`, or a user's row written again while its
    /// devices point to it, failed with "FOREIGN KEY constraint failed").
    pub fn apply_changes(
        &mut self,
        reset: bool,
        changes: &[Change],
        upto: i64,
    ) -> Result<(), String> {
        self.db
            .execute_batch("PRAGMA foreign_keys = OFF")
            .map_err(|e| e.to_string())?;
        let applied = self.apply_in_one(reset, changes, upto);
        let restored = self
            .db
            .execute_batch("PRAGMA foreign_keys = ON")
            .map_err(|e| e.to_string());
        applied.and(restored)
    }

    fn apply_in_one(&mut self, reset: bool, changes: &[Change], upto: i64) -> Result<(), String> {
        let transaction = self.db.transaction().map_err(|e| e.to_string())?;
        if reset {
            for (table, _) in TABLES {
                transaction
                    .execute(&format!("DELETE FROM {table}"), [])
                    .map_err(|e| e.to_string())?;
            }
        }
        for change in changes {
            if change.op == "mark" {
                continue;
            }
            // Only the copied tables, and only their own columns (never a name from the message itself).
            let Some((table, key)) = TABLES.iter().find(|(table, _)| *table == change.table) else {
                return Err(format!("change {} for an unknown table", change.seq));
            };
            let known = columns(&transaction, table)?;
            match change.op.as_str() {
                "delete" => {
                    let key_value = change
                        .key
                        .parse::<i64>()
                        .ok()
                        .filter(|_| *key == "id" && *table == "users")
                        .map_or_else(|| SqlValue::Text(change.key.clone()), SqlValue::Integer);
                    transaction
                        .execute(
                            &format!("DELETE FROM {table} WHERE \"{key}\" = ?1"),
                            [key_value],
                        )
                        .map_err(|e| e.to_string())?;
                }
                "upsert" => {
                    let object: serde_json::Map<String, serde_json::Value> = change
                        .row
                        .as_deref()
                        .and_then(|row| serde_json::from_str(row).ok())
                        .ok_or_else(|| format!("change {} has no row", change.seq))?;
                    let present: Vec<&String> =
                        known.iter().filter(|c| object.contains_key(*c)).collect();
                    if present.is_empty() {
                        return Err(format!("change {} has no known column", change.seq));
                    }
                    let names: Vec<String> = present.iter().map(|c| format!("\"{c}\"")).collect();
                    let slots: Vec<String> = (1..=present.len()).map(|i| format!("?{i}")).collect();
                    let values = present.iter().map(|c| sql_value(&object[*c]));
                    transaction
                        .execute(
                            &format!(
                                "INSERT OR REPLACE INTO {table} ({}) VALUES ({})",
                                names.join(", "),
                                slots.join(", ")
                            ),
                            params_from_iter(values),
                        )
                        .map_err(|e| e.to_string())?;
                }
                other => return Err(format!("change {}: unknown operation {other}", change.seq)),
            }
        }
        if upto > 0 || reset {
            transaction
                .execute(
                    "INSERT INTO replica_state (id, seq) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET seq = excluded.seq",
                    [upto],
                )
                .map_err(|e| e.to_string())?;
        }
        transaction.commit().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> (std::path::PathBuf, Store) {
        let dir =
            std::env::temp_dir().join(format!("queqiao-replica-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir.join("test.db")).unwrap();
        (dir, store)
    }

    /// Everything the standby is sent until it is level, applied as it would apply it.
    fn catch_up(primary: &Store, standby: &mut Store) {
        loop {
            let after = standby.replica_seq().unwrap();
            match primary.next_for_standby(after).unwrap() {
                Batch::Copy { upto, rows } => {
                    assert!(upto >= 1, "a full copy is up to a change");
                    let chunks: Vec<&[Change]> = rows.chunks(BATCH).collect();
                    if chunks.is_empty() {
                        standby.apply_changes(true, &[], upto).unwrap();
                    }
                    for (index, chunk) in chunks.iter().enumerate() {
                        let last = index + 1 == chunks.len();
                        standby
                            .apply_changes(index == 0, chunk, if last { upto } else { 0 })
                            .unwrap();
                    }
                }
                Batch::Changes(changes) if changes.is_empty() => return,
                Batch::Changes(changes) => {
                    let upto = changes.last().unwrap().seq;
                    standby.apply_changes(false, &changes, upto).unwrap();
                }
            }
        }
    }

    fn dump(store: &Store) -> Vec<String> {
        let mut rows = Vec::new();
        for (table, key) in TABLES {
            let columns = columns(&store.db, table).unwrap();
            let mut query = store
                .db
                .prepare(&format!(
                    "SELECT {} FROM {table} AS t ORDER BY \"{key}\"",
                    json_of(&columns, "t")
                ))
                .unwrap();
            let found = query.query_map([], |row| row.get::<_, String>(0)).unwrap();
            rows.extend(found.map(Result::unwrap));
        }
        rows
    }

    #[test]
    fn a_standby_gets_a_full_copy_then_every_change_and_ends_up_the_same() {
        let (dir_a, primary) = temp("primary");
        let (dir_b, mut standby) = temp("standby");
        // Before the standby: a user with a device and a login, and a relay.
        primary.add_user("alice", "correct horse").unwrap();
        let alice = primary.password_record("alice").unwrap().id;
        primary
            .db
            .execute(
                "INSERT INTO devices (id, user_id, name, created, last_seen, public_key) VALUES ('d1', ?1, 'PC', 1, 1, 'aa')",
                [alice],
            )
            .unwrap();
        primary
            .db
            .execute(
                "INSERT INTO refresh_tokens (hash, device_id, role, user_id, expires) VALUES ('h1', 'd1', 'agent', ?1, 9)",
                [alice],
            )
            .unwrap();
        primary.add_relay("shanghai").unwrap();
        catch_up(&primary, &mut standby);
        assert_eq!(dump(&standby), dump(&primary));
        assert!(standby.replica_seq().unwrap() > 0);
        // Then changes: a new user, a renamed device, a login gone, a relay removed: each one follows.
        primary.add_user("bob", "battery staple").unwrap();
        primary
            .db
            .execute("UPDATE devices SET name = 'Laptop' WHERE id = 'd1'", [])
            .unwrap();
        primary
            .db
            .execute("DELETE FROM refresh_tokens WHERE hash = 'h1'", [])
            .unwrap();
        primary.remove_relay("shanghai").unwrap();
        catch_up(&primary, &mut standby);
        assert_eq!(dump(&standby), dump(&primary));
        // The copy logs alice in with her password, as the account server would.
        assert!(
            standby
                .check_password("alice", "correct horse")
                .unwrap()
                .is_some()
        );
        // A standby behind what the log keeps starts again from a full copy (here: the log pruned to nothing).
        primary.add_user("carol", "another horse").unwrap();
        primary.db.execute("DELETE FROM changes", []).unwrap();
        primary.add_user("dave", "yet another horse").unwrap();
        catch_up(&primary, &mut standby);
        assert_eq!(dump(&standby), dump(&primary));
        let _ = std::fs::remove_dir_all(&dir_a);
        let _ = std::fs::remove_dir_all(&dir_b);
    }

    #[test]
    fn a_change_for_anything_but_the_copied_tables_is_refused() {
        let (dir, mut standby) = temp("refuse");
        let bad = Change {
            seq: 1,
            table: "sqlite_master".into(),
            op: "delete".into(),
            key: "x".into(),
            row: None,
        };
        assert!(standby.apply_changes(false, &[bad], 1).is_err());
        let odd_column = Change {
            seq: 2,
            table: "users".into(),
            op: "upsert".into(),
            key: "1".into(),
            row: Some(r#"{"id\" = 1; DROP TABLE users; --": 1}"#.into()),
        };
        assert!(standby.apply_changes(false, &[odd_column], 2).is_err());
        assert!(standby.user_exists("anyone").is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
