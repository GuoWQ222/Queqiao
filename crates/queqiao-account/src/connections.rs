//! The sessions this PC started, for the main window's "Recent connections" of a device:
//! `%APPDATA%\Queqiao\connections.log`, one line per session (the viewer adds it when the session ends), the newest
//! KEEP kept. Nothing secret: when, how long, the other device's id and name. A test build keeps its own file
//! (`connections.test.log`), as it keeps its own saved logins (config.rs).

use std::io::Write;
use std::path::PathBuf;

/// How many sessions the file keeps.
const KEEP: usize = 300;

/// One session this PC started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connection {
    /// When it started, as this PC's local time ("2026-11-23 15:04").
    pub when: String,
    pub seconds: u64,
    pub device_id: String,
    pub device_name: String,
}

fn path() -> Option<PathBuf> {
    let name = if cfg!(feature = "test-files") {
        "connections.test.log"
    } else {
        "connections.log"
    };
    crate::config::folder().ok().map(|folder| folder.join(name))
}

/// This PC's local time now, as `Connection::when` has it.
pub fn local_now() -> String {
    queqiao_core::local_time_minutes()
}

/// Notes a session that ended.
pub fn record(connection: &Connection) {
    let Some(path) = path() else {
        return;
    };
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        // One write per line: the agent and several viewers may add at once.
        let _ = file.write_all(format!("{}\n", line(connection)).as_bytes());
    }
    // Now and then the file is cut back to the newest KEEP.
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 64 * 1024) {
        let mut newest = load();
        newest.reverse();
        let text: Vec<String> = newest.iter().map(line).collect();
        let _ = std::fs::write(&path, text.join("\n") + "\n");
    }
}

/// All of them, newest first.
pub fn load() -> Vec<Connection> {
    let Some(text) = path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Vec::new();
    };
    let mut all: Vec<Connection> = text.lines().filter_map(parse).collect();
    all.reverse();
    all.truncate(KEEP);
    all
}

/// when, seconds, device id, device name: tab-separated (tabs and line breaks in names become spaces).
fn line(connection: &Connection) -> String {
    let clean = |text: &str| text.replace(['\t', '\r', '\n'], " ");
    format!(
        "{}\t{}\t{}\t{}",
        clean(&connection.when),
        connection.seconds,
        clean(&connection.device_id),
        clean(&connection.device_name)
    )
}

fn parse(line: &str) -> Option<Connection> {
    let mut parts = line.splitn(4, '\t');
    let when = parts.next()?.to_string();
    let seconds = parts.next()?.parse().ok()?;
    let device_id = parts.next()?.to_string();
    let device_name = parts.next()?.to_string();
    (!device_id.is_empty()).then_some(Connection {
        when,
        seconds,
        device_id,
        device_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_and_names_cannot_break_them() {
        let connection = Connection {
            when: "2026-11-23 15:04".into(),
            seconds: 754,
            device_id: "sunshine-1234".into(),
            device_name: "SUN\tSHINE\nPC".into(),
        };
        let back = parse(&line(&connection)).unwrap();
        assert_eq!(back.seconds, 754);
        assert_eq!(back.device_id, "sunshine-1234");
        assert_eq!(back.device_name, "SUN SHINE PC");
        assert_eq!(parse("2026-11-23 15:04\tnot a number\tid\tname"), None);
        assert_eq!(parse("2026-11-23 15:04\t3\t\tname"), None);
    }
}
