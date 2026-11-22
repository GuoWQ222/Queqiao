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
/// Integrity levels: the last part of a token's label (S-1-16-...).
pub const MEDIUM_INTEGRITY: u32 = 0x2000;
pub const HIGH_INTEGRITY: u32 = 0x3000;

/// A token handle closed when dropped.
struct Token(windows::Win32::Foundation::HANDLE);

impl Drop for Token {
    fn drop(&mut self) {
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

fn own_token(access: windows::Win32::Security::TOKEN_ACCESS_MASK) -> Option<Token> {
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = windows::Win32::Foundation::HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut token) }.ok()?;
    Some(Token(token))
}

/// A token's integrity level (MEDIUM_INTEGRITY, HIGH_INTEGRITY, ...).
pub fn integrity_level(token: windows::Win32::Foundation::HANDLE) -> Option<u32> {
    use windows::Win32::Security::{
        GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TOKEN_MANDATORY_LABEL,
        TokenIntegrityLevel,
    };
    let mut size = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenIntegrityLevel, None, 0, &mut size) };
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(4)];
    unsafe {
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut size,
        )
    }
    .ok()?;
    let label = unsafe { &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL) };
    let count = unsafe { *GetSidSubAuthorityCount(label.Label.Sid) };
    let last = u32::from(count.checked_sub(1)?);
    Some(unsafe { *GetSidSubAuthority(label.Label.Sid, last) })
}

/// This process's integrity level.
pub fn own_integrity_level() -> Option<u32> {
    integrity_level(own_token(windows::Win32::Security::TOKEN_QUERY)?.0)
}

/// An integrity level's name: Low, Medium, High, System.
pub fn integrity_name(level: u32) -> &'static str {
    match level {
        0..0x1000 => "Untrusted",
        0x1000..0x2000 => "Low",
        0x2000..0x3000 => "Medium",
        0x3000..0x4000 => "High",
        _ => "System",
    }
}

/// Sets a token's integrity level (lowering needs nothing; raising needs SeTcbPrivilege, which the service has).
pub fn set_integrity_level(
    token: windows::Win32::Foundation::HANDLE,
    level: u32,
) -> windows::core::Result<()> {
    use windows::Win32::Security::{
        PSID, SECURITY_MANDATORY_LABEL_AUTHORITY, SID, SID_AND_ATTRIBUTES, SetTokenInformation,
        TOKEN_MANDATORY_LABEL, TokenIntegrityLevel,
    };
    let mut sid = SID {
        Revision: 1,
        SubAuthorityCount: 1,
        IdentifierAuthority: SECURITY_MANDATORY_LABEL_AUTHORITY,
        SubAuthority: [level],
    };
    let label = TOKEN_MANDATORY_LABEL {
        Label: SID_AND_ATTRIBUTES {
            Sid: PSID((&raw mut sid).cast()),
            // SE_GROUP_INTEGRITY.
            Attributes: 0x20,
        },
    };
    unsafe {
        SetTokenInformation(
            token,
            TokenIntegrityLevel,
            (&raw const label).cast(),
            (size_of::<TOKEN_MANDATORY_LABEL>() + size_of::<SID>()) as u32,
        )
    }
}

/// Whether this process runs above the person's own level: at High integrity without being elevated. The service
/// starts the person's agent so (the security review of 2026-11-22): the person's other programs, at Medium, can then
/// not hook it, send its windows messages or open its threads. (An administrator's full token is High already.)
pub fn runs_raised() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    let Some(token) = own_token(TOKEN_QUERY) else {
        return false;
    };
    let mut elevation = TOKEN_ELEVATION::default();
    let mut size = 0u32;
    let elevated = unsafe {
        GetTokenInformation(
            token.0,
            TokenElevation,
            Some((&raw mut elevation).cast()),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut size,
        )
    }
    .map_or(true, |()| elevation.TokenIsElevated != 0);
    !elevated && integrity_level(token.0).is_some_and(|level| level >= HIGH_INTEGRITY)
}

/// The token for the programs this process starts: when it runs raised, a copy at Medium again (the viewer, Explorer
/// and command lines run as the person's programs do; files dragged from Explorer reach the viewer); None: as this
/// process.
fn program_token() -> windows::core::Result<Option<Token>> {
    use windows::Win32::Security::{
        DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY,
        TOKEN_DUPLICATE, TOKEN_QUERY, TokenPrimary,
    };
    if !runs_raised() {
        return Ok(None);
    }
    let own = own_token(TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY)
        .ok_or_else(windows::core::Error::from_thread)?;
    let mut copy = windows::Win32::Foundation::HANDLE::default();
    unsafe {
        DuplicateTokenEx(
            own.0,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut copy,
        )
    }?;
    let copy = Token(copy);
    set_integrity_level(copy.0, MEDIUM_INTEGRITY)?;
    Ok(Some(copy))
}

/// CreateProcessW (no application name, no new environment), at Medium integrity when this process runs raised: how
/// the agent and its command lines start programs.
///
/// # Safety
/// `startup` and `information` as for CreateProcessW: valid for the call, `startup` filled in for `flags`.
pub unsafe fn create_process(
    command_line: windows::core::PWSTR,
    inherit: bool,
    flags: windows::Win32::System::Threading::PROCESS_CREATION_FLAGS,
    folder: windows::core::PCWSTR,
    startup: *const windows::Win32::System::Threading::STARTUPINFOW,
    information: *mut windows::Win32::System::Threading::PROCESS_INFORMATION,
) -> windows::core::Result<()> {
    use windows::Win32::System::Threading::{CreateProcessAsUserW, CreateProcessW};
    use windows::core::PCWSTR;
    match program_token()? {
        Some(token) => unsafe {
            CreateProcessAsUserW(
                Some(token.0),
                PCWSTR::null(),
                Some(command_line),
                None,
                None,
                inherit,
                flags,
                None,
                folder,
                startup,
                information,
            )
        },
        None => unsafe {
            CreateProcessW(
                PCWSTR::null(),
                Some(command_line),
                None,
                None,
                inherit,
                flags,
                None,
                folder,
                startup,
                information,
            )
        },
    }
}

/// Starts `program` with `arguments` (the rest of its command line, each part quote()d) as create_process does; its
/// process id and integrity level.
pub fn start_program(
    program: &std::path::Path,
    arguments: &str,
) -> Result<(u32, Option<u32>), String> {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{
        OpenProcessToken, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows::core::{PCWSTR, PWSTR};
    let mut command: Vec<u16> = format!("{} {arguments}", quote(&program.to_string_lossy()))
        .trim_end()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut information = PROCESS_INFORMATION::default();
    unsafe {
        create_process(
            PWSTR(command.as_mut_ptr()),
            false,
            PROCESS_CREATION_FLAGS(0),
            PCWSTR::null(),
            &startup,
            &mut information,
        )
    }
    .map_err(|e| format!("starting {}: {}", program.display(), e.message()))?;
    let mut token = windows::Win32::Foundation::HANDLE::default();
    let level = unsafe { OpenProcessToken(information.hProcess, TOKEN_QUERY, &mut token) }
        .ok()
        .and_then(|()| {
            let token = Token(token);
            integrity_level(token.0)
        });
    let _ = unsafe { CloseHandle(information.hThread) };
    let _ = unsafe { CloseHandle(information.hProcess) };
    Ok((information.dwProcessId, level))
}

/// A command line's part as a program reads it back (CommandLineToArgvW's rules): quoted when it has spaces or quotes.
pub fn quote(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '\n', '"']) {
        return argument.to_string();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0usize;
    for c in argument.chars() {
        if c == '\\' {
            backslashes += 1;
            continue;
        }
        let times = if c == '"' {
            backslashes * 2 + 1
        } else {
            backslashes
        };
        quoted.extend(std::iter::repeat_n('\\', times));
        quoted.push(c);
        backslashes = 0;
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The privileges of this process's token, by name (SeChangeNotifyPrivilege, ...).
pub fn own_privileges() -> Vec<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::TOKEN_QUERY;
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let mut token = HANDLE::default();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }.is_err() {
        return Vec::new();
    }
    let names = token_privileges(token)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let _ = unsafe { CloseHandle(token) };
    names
}

/// A token's privileges (whether enabled or not): each one's LUID and name.
pub fn token_privileges(
    token: windows::Win32::Foundation::HANDLE,
) -> Vec<(windows::Win32::Foundation::LUID, String)> {
    use windows::Win32::Security::{
        GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeNameW, TOKEN_PRIVILEGES,
        TokenPrivileges,
    };
    use windows::core::{PCWSTR, PWSTR};
    let mut size = 0u32;
    let _ = unsafe { GetTokenInformation(token, TokenPrivileges, None, 0, &mut size) };
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    let read = unsafe {
        GetTokenInformation(
            token,
            TokenPrivileges,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut size,
        )
    };
    if read.is_err() {
        return Vec::new();
    }
    let header = buffer.as_ptr().cast::<TOKEN_PRIVILEGES>();
    let room = (buffer.len() * 8).saturating_sub(4) / size_of::<LUID_AND_ATTRIBUTES>();
    let count = (unsafe { (*header).PrivilegeCount } as usize).min(room);
    let first = unsafe { std::ptr::addr_of!((*header).Privileges) }.cast::<LUID_AND_ATTRIBUTES>();
    (0..count)
        .map(|index| {
            let luid = unsafe { first.add(index).read_unaligned() }.Luid;
            let mut name = [0u16; 80];
            let mut length = name.len() as u32;
            let looked_up = unsafe {
                LookupPrivilegeNameW(
                    PCWSTR::null(),
                    &luid,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                )
            };
            let name = match looked_up {
                Ok(()) => String::from_utf16_lossy(&name[..(length as usize).min(name.len())]),
                Err(_) => format!("LUID {}:{}", luid.HighPart, luid.LowPart),
            };
            (luid, name)
        })
        .collect()
}

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

#[cfg(test)]
mod integrity_tests {
    use super::*;

    #[test]
    fn parts_are_quoted_as_programs_read_them() {
        assert_eq!(quote("plain"), "plain");
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("two words"), r#""two words""#);
        assert_eq!(quote(r"C:\a b\"), r#""C:\a b\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(r#"x\"y z"#), r#""x\\\"y z""#);
    }

    #[test]
    fn a_copy_of_the_token_goes_down_to_medium() {
        use windows::Win32::Security::{
            DuplicateTokenEx, SecurityImpersonation, TOKEN_ALL_ACCESS, TOKEN_DUPLICATE,
            TOKEN_QUERY, TokenPrimary,
        };
        let own = own_token(TOKEN_DUPLICATE | TOKEN_QUERY).unwrap();
        assert!(own_integrity_level().unwrap() >= MEDIUM_INTEGRITY);
        let mut copy = windows::Win32::Foundation::HANDLE::default();
        unsafe {
            DuplicateTokenEx(
                own.0,
                TOKEN_ALL_ACCESS,
                None,
                SecurityImpersonation,
                TokenPrimary,
                &mut copy,
            )
        }
        .unwrap();
        let copy = Token(copy);
        set_integrity_level(copy.0, MEDIUM_INTEGRITY).unwrap();
        assert_eq!(integrity_level(copy.0), Some(MEDIUM_INTEGRITY));
        assert_eq!(integrity_name(MEDIUM_INTEGRITY), "Medium");
        assert_eq!(integrity_name(HIGH_INTEGRITY), "High");
        assert_eq!(integrity_name(0x4000), "System");
    }
}
