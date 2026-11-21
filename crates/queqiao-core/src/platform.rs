//! Process set-up that every Queqiao program on Windows does first.

/// Per-monitor DPI awareness (screen sizes and positions in real pixels) and no power throttling:
/// Windows otherwise runs a windowless process on slow efficiency cores (measured on a 12th-gen Intel
/// laptop: OpenH264 2.7x slower). Returns a note for each step that failed (older Windows).
pub fn prepare_process() -> Vec<String> {
    use windows::Win32::System::Threading::{
        GetCurrentProcess, PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        PROCESS_POWER_THROTTLING_EXECUTION_SPEED, PROCESS_POWER_THROTTLING_STATE,
        ProcessPowerThrottling, SetProcessInformation,
    };
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    let mut notes = Vec::new();
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: 0,
    };
    let throttling = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&raw const state).cast(),
            size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(error) = throttling {
        notes.push(format!("power throttling opt-out: {}", error.message()));
    }
    let dpi = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    if let Err(error) = dpi {
        notes.push(format!("per-monitor DPI awareness: {}", error.message()));
    }
    notes
}

/// When the program panics, a crash report (what panicked, where, and the call stack) goes into
/// %APPDATA%\Queqiao\crashes\<program>-<Unix time>.txt; the newest 20 are kept. Nothing leaves the PC.
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
                env!("CARGO_PKG_VERSION")
            );
            let _ = std::fs::write(folder.join(format!("{program}-{stamp}.txt")), report);
            keep_newest(&folder, 20);
        }
        previous(info);
    }));
}

/// Where crash reports go: %APPDATA%\Queqiao\crashes.
pub fn crash_folder() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(|folder| {
        std::path::PathBuf::from(folder)
            .join("Queqiao")
            .join("crashes")
    })
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
