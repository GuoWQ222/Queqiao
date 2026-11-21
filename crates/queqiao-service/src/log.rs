//! The service's log: `%ProgramData%\Queqiao\service.log` (the folder is set up by `install` so that only
//! SYSTEM and administrators may change it; everyone may read it). At 1 MB it moves to `service.log.old`.
//! Nothing secret goes in it.

use std::io::Write;
use std::path::PathBuf;

pub fn folder() -> PathBuf {
    let base = std::env::var_os("ProgramData").unwrap_or_else(|| r"C:\ProgramData".into());
    PathBuf::from(base).join("Queqiao")
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
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{stamp} [{}] {text}", std::process::id());
    }
}
