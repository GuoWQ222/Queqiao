//! Setting the service up and taking it down (an administrator's job).
//!
//! `install` copies `queqiao-service.exe` and `queqiao-agent.exe` to `%ProgramFiles%\Queqiao` (a service runs its
//! program as SYSTEM, so the program must be where only administrators can change it), sets up
//! `%ProgramData%\Queqiao` for the log (SYSTEM and administrators may change it, everyone may read it), registers
//! the service (automatic start, restarted by Windows if it fails), allows services to send Ctrl+Alt+Del
//! (the SoftwareSASGeneration policy; what it was is kept and put back by `uninstall`) and starts it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SERVICE_EXISTS, ERROR_SERVICE_NOT_ACTIVE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteKeyValueW, RegSetValueExW,
};
use windows::Win32::System::Services::{
    ChangeServiceConfig2W, ChangeServiceConfigW, CloseServiceHandle, ControlService,
    CreateServiceW, DeleteService, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_ACTION,
    SC_ACTION_RESTART, SC_HANDLE, SC_MANAGER_ALL_ACCESS, SC_STATUS_PROCESS_INFO,
    SERVICE_ALL_ACCESS, SERVICE_AUTO_START, SERVICE_CONFIG_DESCRIPTION,
    SERVICE_CONFIG_FAILURE_ACTIONS, SERVICE_CONTROL_STOP, SERVICE_DESCRIPTIONW,
    SERVICE_ERROR_NORMAL, SERVICE_FAILURE_ACTIONSW, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_PROCESS, SERVICE_STOPPED,
    SERVICE_WIN32_OWN_PROCESS, StartServiceW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::launch::wide;
use crate::service::{PARAMETERS, read_dword};

const DISPLAY_NAME: &str = "Queqiao remote control";
const DESCRIPTION: &str = "Lets the Queqiao devices of this PC's people see and use the sign-in, lock, Ctrl+Alt+Del and UAC screens during a session, and starts Queqiao for the person signed in. It has no network connection of its own.";
const PROGRAMS: [&str; 2] = ["queqiao-service.exe", "queqiao-agent.exe"];
const SAS_POLICY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System";
const SAS_VALUE: &str = "SoftwareSASGeneration";
/// Kept in the service's Parameters: SoftwareSASGeneration before install (`ABSENT`: there was none).
const SAS_BEFORE: &str = "SasPolicyBefore";
const ABSENT: u32 = u32::MAX;

struct Handle(SC_HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn manager() -> Result<Handle, String> {
    unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ALL_ACCESS) }
        .map(Handle)
        .map_err(|e| {
            format!(
                "the service manager refused ({}): run this as an administrator",
                e.message()
            )
        })
}

fn open(manager: &Handle, access: u32) -> Result<Option<Handle>, String> {
    let name = wide(crate::NAME);
    match unsafe { OpenServiceW(manager.0, PCWSTR(name.as_ptr()), access) } {
        Ok(handle) => Ok(Some(Handle(handle))),
        Err(error)
            if error.code()
                == windows::Win32::Foundation::ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() =>
        {
            Ok(None)
        }
        Err(error) => Err(format!("OpenServiceW: {}", error.message())),
    }
}

fn state(service: &Handle) -> Result<SERVICE_STATUS_PROCESS, String> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut needed = 0u32;
    let bytes = unsafe {
        std::slice::from_raw_parts_mut(
            (&raw mut status).cast::<u8>(),
            size_of::<SERVICE_STATUS_PROCESS>(),
        )
    };
    unsafe { QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed) }
        .map_err(|e| format!("QueryServiceStatusEx: {}", e.message()))?;
    Ok(status)
}

fn wait_for(service: &Handle, wanted: SERVICE_STATUS_CURRENT_STATE, seconds: u64) -> bool {
    let until = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < until {
        if state(service).is_ok_and(|s| s.dwCurrentState == wanted) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

fn stop(service: &Handle) -> Result<(), String> {
    let mut status = SERVICE_STATUS::default();
    match unsafe { ControlService(service.0, SERVICE_CONTROL_STOP, &mut status) } {
        Ok(()) => {}
        Err(error) if error.code() == ERROR_SERVICE_NOT_ACTIVE.to_hresult() => return Ok(()),
        Err(error) => return Err(format!("stopping the service: {}", error.message())),
    }
    if wait_for(service, SERVICE_STOPPED, 20) {
        Ok(())
    } else {
        Err("the service did not stop within 20 s".into())
    }
}

fn set_value(key: &str, name: &str, data: Value) -> Result<(), String> {
    let (key_name, value_name) = (wide(key), wide(name));
    let mut handle = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_name.as_ptr()),
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
    .map_err(|e| format!("{key}: {}", e.message()))?;
    let result = match data {
        Value::Text(text) => {
            let text = wide(text);
            let bytes =
                unsafe { std::slice::from_raw_parts(text.as_ptr().cast::<u8>(), text.len() * 2) };
            unsafe {
                RegSetValueExW(
                    handle,
                    PCWSTR(value_name.as_ptr()),
                    None,
                    REG_SZ,
                    Some(bytes),
                )
            }
        }
        Value::Number(number) => unsafe {
            RegSetValueExW(
                handle,
                PCWSTR(value_name.as_ptr()),
                None,
                REG_DWORD,
                Some(&number.to_le_bytes()),
            )
        },
    };
    let _ = unsafe { RegCloseKey(handle) };
    result
        .ok()
        .map_err(|e| format!("{key}\\{name}: {}", e.message()))
}

fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let (key_name, value_name) = (wide(key), wide(name));
    let result = unsafe {
        RegDeleteKeyValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key_name.as_ptr()),
            PCWSTR(value_name.as_ptr()),
        )
    };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result
        .ok()
        .map_err(|e| format!("{key}\\{name}: {}", e.message()))
}

enum Value<'a> {
    Text(&'a str),
    Number(u32),
}

fn program_files() -> PathBuf {
    let base = std::env::var_os("ProgramFiles").unwrap_or_else(|| r"C:\Program Files".into());
    PathBuf::from(base).join("Queqiao")
}

/// Copies the programs next to this one into the protected folder (unless they are there already).
fn place_programs(target: &Path) -> Result<(), String> {
    let here = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .parent()
        .map(Path::to_path_buf)
        .ok_or("this program's folder is not known")?;
    std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    let same = |a: &Path, b: &Path| {
        std::fs::canonicalize(a)
            .ok()
            .zip(std::fs::canonicalize(b).ok())
            .is_some_and(|(a, b)| a == b)
    };
    if same(&here, target) {
        return Ok(());
    }
    for program in PROGRAMS {
        let from = here.join(program);
        if !from.is_file() {
            return Err(format!("{} is missing next to this program", program));
        }
        std::fs::copy(&from, target.join(program))
            .map_err(|e| format!("copying {program} to {}: {e}", target.display()))?;
    }
    Ok(())
}

/// The log folder: owned by Administrators; SYSTEM and Administrators may change it, users may read it.
fn secure_log_folder() -> Result<(), String> {
    let folder = crate::log::folder();
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let sddl = wide("O:BAD:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)");
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
    }
    .map_err(|e| format!("security descriptor: {}", e.message()))?;
    let mut owner = PSID::default();
    let mut owner_defaulted = false.into();
    let mut dacl = std::ptr::null_mut();
    let mut present = false.into();
    let mut dacl_defaulted = false.into();
    let result = unsafe {
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut owner_defaulted)
            .and_then(|()| {
                GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut dacl_defaulted)
            })
            .and_then(|()| {
                let path = wide(&folder.display().to_string());
                SetNamedSecurityInfoW(
                    PCWSTR(path.as_ptr()),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION
                        | DACL_SECURITY_INFORMATION
                        | PROTECTED_DACL_SECURITY_INFORMATION,
                    Some(owner),
                    None,
                    Some(dacl),
                    None,
                )
                .ok()
            })
    };
    unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result.map_err(|e| format!("securing {}: {}", folder.display(), e.message()))
}

/// Lets services send Ctrl+Alt+Del, keeping what the policy was.
fn allow_sas() -> Result<String, String> {
    let before = read_dword(SAS_POLICY, SAS_VALUE);
    match before {
        Some(1) | Some(3) => Ok(format!(
            "Ctrl+Alt+Del from services was allowed already (SoftwareSASGeneration {})",
            before.unwrap_or_default()
        )),
        _ => {
            set_value(
                PARAMETERS,
                SAS_BEFORE,
                Value::Number(before.unwrap_or(ABSENT)),
            )?;
            let now = if before == Some(2) { 3 } else { 1 };
            set_value(SAS_POLICY, SAS_VALUE, Value::Number(now))?;
            Ok(format!(
                "Ctrl+Alt+Del from services allowed (SoftwareSASGeneration {} -> {now})",
                before.map_or("not set".to_string(), |v| v.to_string())
            ))
        }
    }
}

fn restore_sas() -> Result<Option<String>, String> {
    let Some(before) = read_dword(PARAMETERS, SAS_BEFORE) else {
        return Ok(None);
    };
    if before == ABSENT {
        delete_value(SAS_POLICY, SAS_VALUE)?;
    } else {
        set_value(SAS_POLICY, SAS_VALUE, Value::Number(before))?;
    }
    Ok(Some(format!(
        "SoftwareSASGeneration put back ({})",
        if before == ABSENT {
            "not set".to_string()
        } else {
            before.to_string()
        }
    )))
}

pub fn install(args: &[String]) -> Result<(), String> {
    let mut agent_arguments = String::new();
    let mut logon_arguments = String::new();
    let mut restart_agent = true;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--agent-arguments" => {
                agent_arguments = rest
                    .next()
                    .ok_or("--agent-arguments needs a value")?
                    .clone()
            }
            "--logon-arguments" => {
                logon_arguments = rest
                    .next()
                    .ok_or("--logon-arguments needs a value")?
                    .clone()
            }
            "--no-restart" => restart_agent = false,
            other => return Err(format!("unknown option {other}")),
        }
    }
    let manager = manager()?;
    let target = program_files();
    // A service already there: stop it (its programs are about to be replaced).
    let existing = open(&manager, SERVICE_ALL_ACCESS)?;
    if let Some(service) = &existing {
        stop(service)?;
    }
    place_programs(&target)?;
    secure_log_folder()?;
    let binary = wide(&format!("\"{}\"", target.join(PROGRAMS[0]).display()));
    let (name, display) = (wide(crate::NAME), wide(DISPLAY_NAME));
    let service = match existing {
        Some(service) => {
            unsafe {
                ChangeServiceConfigW(
                    service.0,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(binary.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR(display.as_ptr()),
                )
            }
            .map_err(|e| format!("ChangeServiceConfigW: {}", e.message()))?;
            service
        }
        None => Handle(
            unsafe {
                CreateServiceW(
                    manager.0,
                    PCWSTR(name.as_ptr()),
                    PCWSTR(display.as_ptr()),
                    SERVICE_ALL_ACCESS,
                    SERVICE_WIN32_OWN_PROCESS,
                    SERVICE_AUTO_START,
                    SERVICE_ERROR_NORMAL,
                    PCWSTR(binary.as_ptr()),
                    PCWSTR::null(),
                    None,
                    PCWSTR::null(),
                    PCWSTR::null(),
                    PCWSTR::null(),
                )
            }
            .map_err(|e| {
                if e.code() == ERROR_SERVICE_EXISTS.to_hresult() {
                    "the service exists already".to_string()
                } else {
                    format!("CreateServiceW: {}", e.message())
                }
            })?,
        ),
    };
    let mut description = wide(DESCRIPTION);
    let info = SERVICE_DESCRIPTIONW {
        lpDescription: PWSTR(description.as_mut_ptr()),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_DESCRIPTION,
            Some((&raw const info).cast()),
        )
    }
    .map_err(|e| format!("service description: {}", e.message()))?;
    // Restarted by Windows 5 s after a failure, three times a day.
    let mut actions = [SC_ACTION {
        Type: SC_ACTION_RESTART,
        Delay: 5000,
    }; 3];
    let failure = SERVICE_FAILURE_ACTIONSW {
        dwResetPeriod: 86_400,
        lpRebootMsg: PWSTR::null(),
        lpCommand: PWSTR::null(),
        cActions: actions.len() as u32,
        lpsaActions: actions.as_mut_ptr(),
    };
    unsafe {
        ChangeServiceConfig2W(
            service.0,
            SERVICE_CONFIG_FAILURE_ACTIONS,
            Some((&raw const failure).cast()),
        )
    }
    .map_err(|e| format!("failure actions: {}", e.message()))?;
    set_value(PARAMETERS, "AgentArguments", Value::Text(&agent_arguments))?;
    set_value(PARAMETERS, "LogonArguments", Value::Text(&logon_arguments))?;
    set_value(
        PARAMETERS,
        "RestartAgent",
        Value::Number(u32::from(restart_agent)),
    )?;
    let sas = allow_sas()?;
    unsafe { StartServiceW(service.0, None) }
        .map_err(|e| format!("starting the service: {}", e.message()))?;
    if !wait_for(&service, SERVICE_RUNNING, 20) {
        return Err("the service did not start within 20 s".into());
    }
    println!(
        "installed: {} (programs in {})",
        crate::NAME,
        target.display()
    );
    println!("{sas}");
    println!("running, process {}", state(&service)?.dwProcessId);
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let manager = manager()?;
    let Some(service) = open(&manager, SERVICE_ALL_ACCESS)? else {
        println!("not installed");
        return Ok(());
    };
    stop(&service)?;
    let restored = restore_sas()?;
    // The device kept for the sign-in screen goes with the service.
    crate::logon::forget();
    unsafe { DeleteService(service.0) }.map_err(|e| format!("DeleteService: {}", e.message()))?;
    println!("removed: {}", crate::NAME);
    if let Some(restored) = restored {
        println!("{restored}");
    }
    println!(
        "the programs stay in {} (the uninstaller removes them)",
        program_files().display()
    );
    Ok(())
}

pub fn status() -> Result<(), String> {
    let name = wide(crate::NAME);
    let manager = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), 0x0001) }
        .map(Handle)
        .map_err(|e| format!("OpenSCManagerW: {}", e.message()))?;
    let service =
        match unsafe { OpenServiceW(manager.0, PCWSTR(name.as_ptr()), SERVICE_QUERY_STATUS) } {
            Ok(handle) => Handle(handle),
            Err(_) => {
                println!("not installed");
                return Ok(());
            }
        };
    let status = state(&service)?;
    println!(
        "{}: state {}, process {}",
        crate::NAME,
        status.dwCurrentState.0,
        status.dwProcessId
    );
    Ok(())
}
