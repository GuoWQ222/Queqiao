//! Process set-up that every Queqiao program on Linux does first (docs/LINUX.md), with the names of Windows'
//! platform.rs: the crash log. Windows' DPI awareness and power-throttling opt-out have no Linux counterpart (X11 and
//! the scheduler give a process real pixels and full-speed cores already).

/// Nothing to set on Linux; returns the notes Windows gives for steps that failed (none).
pub fn prepare_process() -> Vec<String> {
    Vec::new()
}

/// When the program panics, a crash report (what panicked, where, and the call stack) goes into
/// `$XDG_STATE_HOME/queqiao/crashes/<program>-<Unix time>.txt` (~/.local/state/queqiao/crashes); the newest 20 are
/// kept. Nothing leaves the PC.
pub fn install_crash_log(program: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        if let Some(folder) = crash_folder() {
            let _ = std::fs::create_dir_all(&folder);
            let report = format!(
                "{program} {} crashed (Unix time {stamp})\n{info}\n\n{backtrace}\n",
                crate::VERSION
            );
            let _ = std::fs::write(folder.join(format!("{program}-{stamp}.txt")), report);
            keep_newest(&folder, 20);
        }
        previous(info);
    }));
}

/// A folder only this user can reach, for the agent's single-instance lock and its "show your window" socket:
/// $XDG_RUNTIME_DIR/queqiao (the sign-in session's own folder), else ~/.local/state/queqiao/run. None when it is not
/// private: owned by someone else, or others may write in it (another user's program could then stand in for Queqiao).
pub fn runtime_folder() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    let folder = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .map(|path| path.join("queqiao"))
        .or_else(|| Some(crash_folder()?.parent()?.join("run")))?;
    let _ = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder);
    // This process's own user: /proc/self belongs to it.
    let me = std::fs::metadata("/proc/self").ok()?.uid();
    let found = std::fs::symlink_metadata(&folder).ok()?;
    (found.is_dir() && found.uid() == me && found.mode() & 0o077 == 0).then_some(folder)
}

/// Where crash reports go: `$XDG_STATE_HOME/queqiao/crashes`, else ~/.local/state/queqiao/crashes.
pub fn crash_folder() -> Option<std::path::PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| std::path::PathBuf::from(home).join(".local").join("state"))
        })?;
    Some(state.join("queqiao").join("crashes"))
}

fn keep_newest(folder: &std::path::Path, count: usize) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut reports: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    reports.sort();
    let extra = reports.len().saturating_sub(count);
    for (_, path) in reports.into_iter().take(extra) {
        let _ = std::fs::remove_file(path);
    }
}
