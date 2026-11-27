//! Start with Windows: the agent in the user's Run key, so it starts at sign-in and goes online with the saved
//! login; and one agent per user session (a second one, started by hand, leaves at once).

use windows::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_FILE_NOT_FOUND, GetLastError, HANDLE,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegGetValueW, RegSetValueExW,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{PCWSTR, w};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const VALUE: &str = "Queqiao";

/// Whether this agent (this program file) starts with Windows.
pub fn enabled() -> bool {
    read(RUN_KEY, VALUE).is_some_and(|command| command.eq_ignore_ascii_case(&this_command()))
}

pub fn set(on: bool) -> Result<(), String> {
    set_in(RUN_KEY, VALUE, on)
}

fn this_command() -> String {
    match std::env::current_exe() {
        Ok(program) => format!("\"{}\"", program.display()),
        Err(_) => String::new(),
    }
}

fn set_in(key: &str, value: &str, on: bool) -> Result<(), String> {
    if on {
        let command = this_command();
        if command.is_empty() {
            return Err("this program's path is not known".into());
        }
        write(key, value, &command)
    } else {
        delete(key, value)
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn read(key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    result.ok().ok()?;
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
}

fn write(key: &str, value: &str, data: &str) -> Result<(), String> {
    let (key, value, data) = (wide(key), wide(value), wide(data));
    let mut handle = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut handle,
            None,
        )
    }
    .ok()
    .map_err(|e| e.message())?;
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    let result =
        unsafe { RegSetValueExW(handle, PCWSTR(value.as_ptr()), None, REG_SZ, Some(bytes)) };
    let _ = unsafe { RegCloseKey(handle) };
    result.ok().map_err(|e| e.message())
}

fn delete(key: &str, value: &str) -> Result<(), String> {
    let (key, value) = (wide(key), wide(value));
    let result = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result.ok().map_err(|e| e.message())
}

/// Whether the Queqiao service runs on this PC (it then starts Queqiao for the person signed in).
/// Asks the Queqiao service to start its Queqiao at the screen now ([`queqiao_local::CONTROL_START`]): what a Queqiao
/// started by hand does when the service runs but no Queqiao runs here (after Quit).
pub fn ask_service_to_start() -> Result<(), String> {
    use windows::Win32::System::Services::{
        CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT,
        SERVICE_STATUS, SERVICE_USER_DEFINED_CONTROL,
    };
    unsafe {
        let manager = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| format!("the service manager: {}", e.message()))?;
        let result = match OpenServiceW(manager, w!("Queqiao"), SERVICE_USER_DEFINED_CONTROL) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let sent = ControlService(service, queqiao_local::CONTROL_START, &mut status)
                    .map_err(|e| format!("the Queqiao service: {}", e.message()));
                let _ = CloseServiceHandle(service);
                sent
            }
            Err(e) => Err(format!("the Queqiao service: {}", e.message())),
        };
        let _ = CloseServiceHandle(manager);
        result
    }
}

pub fn service_runs() -> bool {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
        SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS,
    };
    unsafe {
        let Ok(manager) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else {
            return false;
        };
        let running = match OpenServiceW(manager, w!("Queqiao"), SERVICE_QUERY_STATUS) {
            Ok(service) => {
                let mut status = SERVICE_STATUS::default();
                let running = QueryServiceStatus(service, &mut status).is_ok()
                    && status.dwCurrentState == SERVICE_RUNNING;
                let _ = CloseServiceHandle(service);
                running
            }
            Err(_) => false,
        };
        let _ = CloseServiceHandle(manager);
        running
    }
}

/// The error when another agent runs in this session.
pub const ALREADY_RUNNING: &str = "Queqiao is running already";

/// Claims "the agent of this user session"; an error when another agent has it. Kept for the process's life.
pub fn claim_single_instance() -> Result<(), String> {
    let mutex: HANDLE =
        unsafe { CreateMutexW(None, false, w!("Local\\QueqiaoAgent")) }.map_err(|e| e.message())?;
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        return Err(ALREADY_RUNNING.into());
    }
    // Never closed: Windows frees it when this process ends.
    let _ = mutex;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Registry::RegDeleteKeyW;

    /// A key of our own, never the real Run key: a test must not make anything start with Windows.
    const TEST_KEY: &str = r"Software\Queqiao\Test";

    #[test]
    fn the_run_entry_is_written_read_and_removed() {
        let _ = delete(TEST_KEY, "AutostartTest");
        assert_eq!(read(TEST_KEY, "AutostartTest"), None);
        set_in(TEST_KEY, "AutostartTest", true).unwrap();
        assert_eq!(read(TEST_KEY, "AutostartTest"), Some(this_command()));
        set_in(TEST_KEY, "AutostartTest", false).unwrap();
        assert_eq!(read(TEST_KEY, "AutostartTest"), None);
        // Removing what is not there is fine.
        set_in(TEST_KEY, "AutostartTest", false).unwrap();
        // The test key goes too, and only it: Software\Queqiao holds the person's own values (the update settings), and
        // RegDeleteKeyW deletes a key with values (only subkeys stop it), so it took them along on a PC with Queqiao
        // installed (found 2026-11-23).
        let key = wide(TEST_KEY);
        let _ = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, PCWSTR(key.as_ptr())) };
    }

    #[test]
    fn a_second_agent_is_refused() {
        claim_single_instance().unwrap();
        assert!(claim_single_instance().is_err());
    }
}
