//! What the controlling device may ask of this PC besides input: lock it, or restart it. Ctrl+Alt+Del (the
//! secure attention sequence) needs the Queqiao service, which runs as SYSTEM (M5).

use windows::Win32::Foundation::{CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Shutdown::{
    InitiateSystemShutdownExW, LockWorkStation, SHTDN_REASON_FLAG_PLANNED,
    SHTDN_REASON_MAJOR_OTHER, SHUTDOWN_REASON,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, w};

/// Seconds Windows shows its restart notice before restarting.
pub const RESTART_DELAY: u32 = 10;

pub fn lock() -> Result<(), String> {
    unsafe { LockWorkStation() }.map_err(|e| e.message())
}

/// Restarts the PC after [`RESTART_DELAY`] seconds; programs may still ask to save their work.
pub fn restart() -> Result<(), String> {
    enable_shutdown_privilege()?;
    unsafe {
        InitiateSystemShutdownExW(
            PCWSTR::null(),
            w!("Queqiao: this PC restarts at the request of the controlling device."),
            RESTART_DELAY,
            false,
            true,
            SHUTDOWN_REASON(SHTDN_REASON_MAJOR_OTHER.0 | SHTDN_REASON_FLAG_PLANNED.0),
        )
    }
    .map_err(|e| e.message())
}

/// A process may shut Windows down only with this privilege enabled in its token (users have it, disabled).
fn enable_shutdown_privilege() -> Result<(), String> {
    let mut token = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
    }
    .map_err(|e| format!("process token: {}", e.message()))?;
    let mut luid = LUID::default();
    unsafe { LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid) }
        .map_err(|e| format!("shutdown privilege: {}", e.message()))?;
    let mut privileges = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        ..Default::default()
    };
    privileges.Privileges[0].Luid = luid;
    privileges.Privileges[0].Attributes = SE_PRIVILEGE_ENABLED;
    let adjusted =
        unsafe { AdjustTokenPrivileges(token, false, Some(&raw const privileges), 0, None, None) };
    // Success can still mean the privilege was not granted (the account does not hold it).
    let not_granted = unsafe { GetLastError() } == ERROR_NOT_ALL_ASSIGNED;
    let _ = unsafe { CloseHandle(token) };
    adjusted.map_err(|e| format!("enabling the shutdown privilege: {}", e.message()))?;
    if not_granted {
        return Err("this account may not restart the PC".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Only the privilege, never a restart: the account running the tests holds it (disabled) like any user.
    #[test]
    fn the_shutdown_privilege_can_be_enabled() {
        super::enable_shutdown_privilege().unwrap();
    }
}
