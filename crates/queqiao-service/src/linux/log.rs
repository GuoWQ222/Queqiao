//! The service's log: standard error, which systemd's journal keeps (`journalctl -u queqiao`). The service and its
//! helper write to it at once, so each line goes in one write. Nothing secret goes in it.

use std::io::Write;

pub fn line(text: &str) {
    let line = format!("[{}] {text}\n", std::process::id());
    let _ = std::io::stderr().lock().write_all(line.as_bytes());
}
