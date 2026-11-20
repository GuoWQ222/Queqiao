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
