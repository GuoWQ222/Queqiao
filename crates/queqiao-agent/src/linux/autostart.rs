//! "Start when I sign in" on Linux (Windows: autostart.rs and its Run key): an XDG autostart entry,
//! `~/.config/autostart/queqiao-agent.desktop` (or under $XDG_CONFIG_HOME), which every desktop following the
//! freedesktop autostart specification starts at sign-in.

use std::path::PathBuf;

fn entry() -> Option<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(config.join("autostart").join("queqiao-agent.desktop"))
}

fn this_command() -> Option<String> {
    let program = std::env::current_exe().ok()?;
    // Exec= quoting (the desktop entry specification): the path in double quotes, with \ " ` $ escaped.
    let escaped: String = program
        .to_string_lossy()
        .chars()
        .flat_map(|c| match c {
            '\\' | '"' | '`' | '$' => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    Some(format!("\"{escaped}\""))
}

/// Whether this Queqiao starts at sign-in (its entry names this program).
pub fn enabled() -> bool {
    let (Some(entry), Some(command)) = (entry(), this_command()) else {
        return false;
    };
    std::fs::read_to_string(entry).is_ok_and(|text| {
        text.lines().any(|line| {
            line.strip_prefix("Exec=")
                .is_some_and(|exec| exec.trim() == command)
        }) && !text.lines().any(|line| line.trim() == "Hidden=true")
    })
}

pub fn set(on: bool) -> Result<(), String> {
    let entry = entry().ok_or("no home folder")?;
    if !on {
        return match std::fs::remove_file(&entry) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        };
    }
    let command = this_command().ok_or("this program's path is not known")?;
    if let Some(folder) = entry.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Queqiao\nComment=Queqiao remote control\nExec={command}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );
    std::fs::write(&entry, text).map_err(|e| e.to_string())
}

/// Whether the Queqiao service runs (its process number in /run/queqiao, as systemd's runtime folder for it, and that
/// process still there): then Queqiao for the person signed in is the one it starts.
pub fn service_runs() -> bool {
    std::fs::read_to_string("/run/queqiao/service.pid")
        .ok()
        .and_then(|pid| pid.trim().parse::<i32>().ok())
        .is_some_and(|pid| {
            pid > 1
                && (unsafe { libc::kill(pid, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM))
        })
}

/// The error when another agent runs for this user.
pub const ALREADY_RUNNING: &str = "Queqiao is running already";

/// Claims "the agent of this user" (a lock on agent.lock in the private runtime folder); an error when another agent
/// has it. Kept for the process's life: the system lets go of it when the process ends, however it ends.
pub fn claim_single_instance() -> Result<(), String> {
    let folder = queqiao_core::platform::runtime_folder().ok_or(
        "no private folder for the single-instance lock ($XDG_RUNTIME_DIR or ~/.local/state)",
    )?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(folder.join("agent.lock"))
        .map_err(|e| e.to_string())?;
    match file.try_lock() {
        Ok(()) => {
            std::mem::forget(file);
            Ok(())
        }
        Err(std::fs::TryLockError::WouldBlock) => Err(ALREADY_RUNNING.into()),
        Err(std::fs::TryLockError::Error(error)) => Err(error.to_string()),
    }
}
