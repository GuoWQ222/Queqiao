//! `--service-update`: run as SYSTEM by the Queqiao service for an update it checked itself (the release key's
//! signature, the SHA-256, a later version). The service stopped, its programs in `%ProgramFiles%\Queqiao` replaced
//! (the earlier ones kept as `Queqiao.old` until the new ones work), the service started again; if its new agent
//! does not keep running, the earlier programs are put back and the service started with them.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_SERVICE_ALREADY_RUNNING, ERROR_SERVICE_NOT_ACTIVE, HANDLE, WAIT_TIMEOUT,
};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP, SERVICE_STOPPED,
    StartServiceW,
};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject};
use windows::core::{PCWSTR, w};

use crate::install::{processes_from, program_files};
use crate::payload::Entry;

/// The service's programs, as `queqiao-service install` puts them in %ProgramFiles%\Queqiao.
const SERVICE_PROGRAMS: [&str; 2] = ["queqiao-service.exe", "queqiao-agent.exe"];
/// How long the service's new agent has to appear.
const APPEAR: Duration = Duration::from_secs(30);
/// How long it must then keep running.
const KEEP: Duration = Duration::from_secs(8);

pub fn service_update(entries: &[Entry], lines: &mut Vec<String>) -> Result<(), String> {
    for program in SERVICE_PROGRAMS {
        if !entries.iter().any(|entry| entry.name == program) {
            return Err(format!("this installer does not carry {program}"));
        }
    }
    let folder = program_files();
    if !folder.join(SERVICE_PROGRAMS[0]).is_file() {
        return Err(format!(
            "the Queqiao service's programs are not in {}",
            folder.display()
        ));
    }
    let old = folder.with_file_name("Queqiao.old");
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    }
    control(false)?;
    lines.push("the Queqiao service stopped".into());
    if let Err(error) = retry(|| std::fs::rename(&folder, &old)) {
        let _ = control(true);
        return Err(format!("keeping the earlier programs: {error}"));
    }
    let outcome = place(entries, &folder)
        .and_then(|()| control(true))
        .and_then(|()| watch_agent(&folder));
    match outcome {
        Ok(note) => {
            lines.push(format!("programs in {}", folder.display()));
            lines.push(note);
            let _ = std::fs::remove_dir_all(&old);
            lines.push("the earlier programs were replaced".into());
            // The display driver of this version (updated when its version changed); Queqiao works without it.
            let _ = crate::display_driver::install_reported(entries, lines);
            Ok(())
        }
        Err(error) => {
            lines.push(format!("the new version did not keep running: {error}"));
            let _ = control(false);
            let _ = retry(|| std::fs::remove_dir_all(&folder));
            if let Err(problem) = retry(|| std::fs::rename(&old, &folder)) {
                return Err(format!(
                    "the update did not keep running ({error}), and the earlier programs could not be put back: {problem}"
                ));
            }
            let started = control(true);
            lines.push(match &started {
                Ok(()) => "the earlier version is back".into(),
                Err(problem) => format!("the earlier version is back, but the service: {problem}"),
            });
            Err(format!(
                "the update did not keep running ({error}): the earlier version is back"
            ))
        }
    }
}

fn place(entries: &[Entry], folder: &Path) -> Result<(), String> {
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    for entry in entries
        .iter()
        .filter(|entry| SERVICE_PROGRAMS.contains(&entry.name.as_str()))
    {
        let path = folder.join(&entry.name);
        std::fs::write(&path, &entry.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Again and again for up to 10 s (the stopped service's processes may hold their files a moment longer).
fn retry(mut action: impl FnMut() -> std::io::Result<()>) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match action() {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => return Err(error.to_string()),
            Err(_) => std::thread::sleep(Duration::from_millis(300)),
        }
    }
}

/// The service's new agent appears within [`APPEAR`] and keeps running for [`KEEP`]. With nobody signed in there
/// may be no agent to start (no device kept for the sign-in screen): then the service running is enough.
fn watch_agent(folder: &Path) -> Result<String, String> {
    let deadline = Instant::now() + APPEAR;
    while Instant::now() < deadline {
        // The agent only: the service itself runs from the same folder.
        let agents = processes_from(folder).into_iter().filter(|(_, path)| {
            path.to_ascii_lowercase()
                .ends_with(&format!("\\{}", SERVICE_PROGRAMS[1]))
        });
        for (pid, _) in agents {
            let Ok(process) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
                continue;
            };
            let kept =
                unsafe { WaitForSingleObject(process, KEEP.as_millis() as u32) } == WAIT_TIMEOUT;
            let _ = unsafe { CloseHandle(process) };
            if kept {
                return Ok(format!(
                    "the service's agent runs (process {pid}, still after {} s)",
                    KEEP.as_secs()
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if someone_signed_in() {
        Err(format!(
            "no agent of the service kept running within {} s",
            APPEAR.as_secs()
        ))
    } else {
        Ok("the service runs (nobody is signed in, no agent to start)".into())
    }
}

fn someone_signed_in() -> bool {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX {
        return false;
    }
    let mut token = HANDLE::default();
    let signed_in = unsafe { WTSQueryUserToken(session, &mut token) }.is_ok();
    if signed_in {
        let _ = unsafe { CloseHandle(token) };
    }
    signed_in
}

struct Service(SC_HANDLE);

impl Drop for Service {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

/// Starts (true) or stops the Queqiao service and waits for it (30 s).
fn control(start: bool) -> Result<(), String> {
    let manager = Service(
        unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
            .map_err(|e| format!("service manager: {}", e.message()))?,
    );
    let service = Service(
        unsafe {
            OpenServiceW(
                manager.0,
                w!("Queqiao"),
                SERVICE_START | SERVICE_STOP | SERVICE_QUERY_STATUS,
            )
        }
        .map_err(|e| format!("the Queqiao service: {}", e.message()))?,
    );
    let mut status = SERVICE_STATUS::default();
    if start {
        match unsafe { StartServiceW(service.0, None) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_SERVICE_ALREADY_RUNNING.to_hresult() => {}
            Err(error) => return Err(format!("starting the service: {}", error.message())),
        }
    } else {
        match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } {
            Ok(()) => {}
            Err(error) if error.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => {}
            Err(error) => return Err(format!("stopping the service: {}", error.message())),
        }
    }
    let wanted: SERVICE_STATUS_CURRENT_STATE = if start {
        SERVICE_RUNNING
    } else {
        SERVICE_STOPPED
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        unsafe { QueryServiceStatus(service.0, &mut status) }
            .map_err(|e| format!("the service's state: {}", e.message()))?;
        if status.dwCurrentState == wanted {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the service did not {} within 30 s",
                if start { "start" } else { "stop" }
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
