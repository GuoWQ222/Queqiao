//! The service's log: `%ProgramData%\Queqiao\service.log` (the folder is set up by `install` so that only
//! SYSTEM and administrators may change it; everyone may read it). At 1 MB it moves to `service.log.old`.
//! Nothing secret goes in it.

use std::io::Write;
use std::path::{Path, PathBuf};

/// `ProgramData\Queqiao`, the machine's folder from the registry (never from environment variables).
pub fn folder() -> PathBuf {
    queqiao_core::platform::program_data().join("Queqiao")
}

pub fn line(text: &str) {
    let path = folder().join("service.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = std::fs::rename(&path, path.with_extension("log.old"));
    }
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds
    );
    append(&path, &format!("{stamp} [{}] {text}\n", std::process::id()));
}

/// Appends `line` in one write. The service and its desktop helper (two processes) write to the same log; a line
/// written in pieces, as `writeln!` does on a `File`, got mixed into the other's (seen on the test laptop on
/// 2026-11-23: "10:58:19.140 [26440] 10:58:19.139desktop helper: started ...").
fn append(path: &Path, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn lines_from_several_writers_stay_whole() {
        let path =
            std::env::temp_dir().join(format!("queqiao_log_test_{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let tail = "x".repeat(40);
        let writers: Vec<_> = (0..8)
            .map(|writer| {
                let (path, tail) = (path.clone(), tail.clone());
                std::thread::spawn(move || {
                    for n in 0..200 {
                        super::append(&path, &format!("writer {writer} line {n} {tail}\n"));
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 8 * 200);
        for line in lines {
            assert!(
                line.starts_with("writer ")
                    && line.ends_with(&tail)
                    && line.matches("writer").count() == 1,
                "a line mixed with another: {line}"
            );
        }
    }
}
