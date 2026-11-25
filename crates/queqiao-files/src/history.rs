//! The files sent and received in sessions, for the main window's Transfers page: `%APPDATA%\Queqiao\transfers.log`,
//! one line per file (the agent and the viewer both add to it), the newest KEEP kept; `transfers.test.log` in a test
//! build. Nothing secret: names, sizes,
//! the other device's name, the time.

use std::io::Write;
use std::path::PathBuf;

use crate::Outcome;

/// How many files the list keeps.
const KEEP: usize = 200;

/// One file of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Received here (else sent from here).
    pub received: bool,
    pub name: String,
    pub size: u64,
    /// The other device's name.
    pub peer: String,
    /// When, as this PC's local time ("2026-11-22 15:04").
    pub when: String,
}

fn path() -> Option<PathBuf> {
    // A test build keeps its own list (the agent's and viewer's test-switches), never the person's.
    let name = if cfg!(feature = "test-files") {
        "transfers.test.log"
    } else {
        "transfers.log"
    };
    std::env::var_os("APPDATA").map(|folder| PathBuf::from(folder).join("Queqiao").join(name))
}

/// Notes a finished file (failed ones are not listed).
pub fn record(outcome: &Outcome, peer: &str) {
    let (received, name, size) = match outcome {
        Outcome::Sent { name, size, .. } => (false, name.clone(), *size),
        Outcome::Received { path, size, .. } => (
            true,
            path.file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
            *size,
        ),
        Outcome::Failed { .. } => return,
    };
    let entry = Entry {
        received,
        name,
        size,
        peer: peer.to_string(),
        when: local_now(),
    };
    let Some(path) = path() else {
        return;
    };
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = file.write_all(format!("{}\n", line(&entry)).as_bytes());
    }
    // Now and then the file is cut back to the newest KEEP.
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > 64 * 1024) {
        let newest: Vec<String> = load().iter().rev().map(line).collect();
        let _ = std::fs::write(&path, newest.join("\n") + "\n");
    }
}

/// The list, newest first.
pub fn load() -> Vec<Entry> {
    let Some(text) = path().and_then(|path| std::fs::read_to_string(path).ok()) else {
        return Vec::new();
    };
    let mut entries: Vec<Entry> = text.lines().filter_map(parse).collect();
    entries.reverse();
    entries.truncate(KEEP);
    entries
}

/// Forgets the list.
pub fn clear() {
    if let Some(path) = path() {
        let _ = std::fs::remove_file(path);
    }
}

/// "12.4 MB", "830 KB", "512 bytes".
pub fn size_text(size: u64) -> String {
    match size {
        0..1024 => format!("{size} bytes"),
        1024..1_048_576 => format!("{:.0} KB", size as f64 / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1} MB", size as f64 / 1_048_576.0),
        _ => format!("{:.2} GB", size as f64 / 1_073_741_824.0),
    }
}

/// when, R or S, size, peer, name: tab-separated (tabs and line breaks in names become spaces).
fn line(entry: &Entry) -> String {
    let clean = |text: &str| text.replace(['\t', '\r', '\n'], " ");
    format!(
        "{}\t{}\t{}\t{}\t{}",
        clean(&entry.when),
        if entry.received { "R" } else { "S" },
        entry.size,
        clean(&entry.peer),
        clean(&entry.name)
    )
}

fn parse(line: &str) -> Option<Entry> {
    let mut parts = line.splitn(5, '\t');
    let when = parts.next()?.to_string();
    let received = match parts.next()? {
        "R" => true,
        "S" => false,
        _ => return None,
    };
    let size = parts.next()?.parse().ok()?;
    let peer = parts.next()?.to_string();
    let name = parts.next()?.to_string();
    Some(Entry {
        received,
        name,
        size,
        peer,
        when,
    })
}

fn local_now() -> String {
    queqiao_core::local_time_minutes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_round_trip_and_names_cannot_break_them() {
        let entry = Entry {
            received: true,
            name: "report\tfinal\n.pdf".into(),
            size: 12_345_678,
            peer: "Office PC".into(),
            when: "2026-11-22 15:04".into(),
        };
        let back = parse(&line(&entry)).unwrap();
        assert_eq!(back.name, "report final .pdf");
        assert_eq!(
            (back.received, back.size, back.peer.as_str()),
            (true, 12_345_678, "Office PC")
        );
        assert!(parse("garbage").is_none());
        assert_eq!(size_text(512), "512 bytes");
        assert_eq!(size_text(12_345_678), "11.8 MB");
    }
}
