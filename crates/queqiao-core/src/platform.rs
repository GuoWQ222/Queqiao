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
                crate::VERSION
            );
            let _ = std::fs::write(folder.join(format!("{program}-{stamp}.txt")), report);
            keep_newest(&folder, 20);
        }
        previous(info);
    }));
}

/// DLLs loaded by name come from System32 only, never from the program's own folder or the current one: for the
/// programs that run as an administrator or as SYSTEM (the installer, the service) wherever their file lies. Their
/// own imports are bound to System32 at link time (/DEPENDENTLOADFLAG, .cargo\config.toml).
pub fn system32_dlls_only() {
    use windows::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_SYSTEM32, SetDefaultDllDirectories,
    };
    let _ = unsafe { SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_SYSTEM32) };
}

/// Whether this process runs as LocalService (S-1-5-19). The Queqiao service starts the agent for the sign-in
/// screen so; no program of a person can.
pub fn runs_as_local_service() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, IsWellKnownSid, TOKEN_QUERY, TOKEN_USER, TokenUser, WinLocalServiceSid,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return false;
    }
    let mut size = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenUser, None, 0, &mut size) };
    // u64s: TOKEN_USER holds a pointer.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut size,
        )
    };
    let _ = unsafe { CloseHandle(token) };
    if read.is_err() {
        return false;
    }
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    unsafe { IsWellKnownSid(user.User.Sid, WinLocalServiceSid) }.as_bool()
}

/// The machine's Program Files folder (HKLM ...\CurrentVersion\ProgramFilesDir). Never from environment variables:
/// an elevated program inherits the person's environment, which any program of theirs can change
/// (HKCU\Environment), as the security review of 2026-11-22 noted.
pub fn program_files() -> std::path::PathBuf {
    machine_folder(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion",
        "ProgramFilesDir",
    )
    .unwrap_or_else(|| system_drive().join("Program Files"))
}

/// The machine's ProgramData folder (HKLM ...\ProfileList\ProgramData, "%SystemDrive%\ProgramData" there, the drive
/// taken from Windows' own folder), never from environment variables.
pub fn program_data() -> std::path::PathBuf {
    machine_folder(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList",
        "ProgramData",
    )
    .unwrap_or_else(|| system_drive().join("ProgramData"))
}

/// The drive Windows is on ("C:\"), from Windows itself (GetSystemWindowsDirectoryW).
fn system_drive() -> std::path::PathBuf {
    use windows::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
    let mut buffer = [0u16; 260];
    let length = unsafe { GetSystemWindowsDirectoryW(Some(&mut buffer)) } as usize;
    let windows = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
    match windows.get(..2) {
        Some(drive) if drive.ends_with(':') => std::path::PathBuf::from(format!("{drive}\\")),
        _ => std::path::PathBuf::from("C:\\"),
    }
}

/// A folder path from HKLM, "%SystemDrive%" in it replaced by the drive Windows is on; None for anything else
/// with a variable in it, or a value that is not there.
fn machine_folder(key: &str, value: &str) -> Option<std::path::PathBuf> {
    use windows::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ, RegGetValueW,
    };
    use windows::core::PCWSTR;
    let key: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    let value: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
    let mut buffer = [0u16; 520];
    let mut size = (buffer.len() * 2) as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    if status.is_err() {
        return None;
    }
    let length = (size as usize / 2).min(buffer.len());
    let text = String::from_utf16_lossy(&buffer[..length]);
    let text = text.trim_end_matches('\0');
    let drive = system_drive();
    let drive = drive.to_string_lossy();
    let text = match text.get(..13) {
        Some(start) if start.eq_ignore_ascii_case("%SystemDrive%") => {
            format!("{}{}", drive.trim_end_matches('\\'), &text[13..])
        }
        _ => text.to_string(),
    };
    (!text.contains('%') && text.len() > 3).then(|| std::path::PathBuf::from(text))
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

#[cfg(test)]
mod folder_tests {
    use super::*;

    #[test]
    fn machine_folders_come_from_the_registry_not_the_environment() {
        let (files, data) = (program_files(), program_data());
        assert!(
            files.ends_with("Program Files") && files.is_dir(),
            "{}",
            files.display()
        );
        assert!(
            data.ends_with("ProgramData") && data.is_dir(),
            "{}",
            data.display()
        );
        // A program of the person can set these for an elevated program it starts: they must not count.
        unsafe {
            std::env::set_var("ProgramFiles", r"C:\Users\Public\fake");
            std::env::set_var("ProgramData", r"C:\Users\Public\fake");
            std::env::set_var("SystemDrive", "Z:");
        }
        assert_eq!(program_files(), files);
        assert_eq!(program_data(), data);
    }
}
