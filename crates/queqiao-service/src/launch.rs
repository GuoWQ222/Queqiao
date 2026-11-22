//! Starting processes in the console session: the desktop helper with a copy of the service's own SYSTEM token
//! moved to that session, the agent with the signed-in person's token.
//!
//! Each process gets exactly the handles listed (PROC_THREAD_ATTRIBUTE_HANDLE_LIST), starts suspended until it
//! is in its job (the job ends everything in it when the service closes it or dies), and the agent's process and
//! first thread can be opened only by SYSTEM for more than waiting for them or ending them: not by its own user
//! either, so no other program of that person can copy its handles (the channel to the helper) or write into it.

use std::path::Path;

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    CREATE_RESTRICTED_TOKEN_FLAGS, CreateRestrictedToken, DuplicateTokenEx, GetTokenInformation,
    LOGON32_LOGON_SERVICE, LOGON32_PROVIDER_DEFAULT, LUID_AND_ATTRIBUTES, LogonUserW,
    PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, SecurityImpersonation, SetTokenInformation,
    TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID, TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY,
    TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenPrimary, TokenSessionId, TokenUser,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_ACTIVE_PROCESS,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
use windows::Win32::System::Threading::{
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW, DETACHED_PROCESS,
    DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, GetCurrentProcess,
    GetExitCodeProcess, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
    OpenProcessToken, PROCESS_INFORMATION, ResumeThread, STARTUPINFOEXW, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// PROC_THREAD_ATTRIBUTE_HANDLE_LIST.
const HANDLE_LIST: usize = 0x0002_0002;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// A handle closed when dropped.
pub struct Owned(pub HANDLE);

unsafe impl Send for Owned {}

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.0.is_invalid() && !self.0.0.is_null() {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }
}

/// The service's own token (SYSTEM), as a primary token for `session`.
pub fn system_token(session: u32) -> Result<Owned, String> {
    let mut own = HANDLE::default();
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_DUPLICATE
                | TOKEN_QUERY
                | TOKEN_ASSIGN_PRIMARY
                | TOKEN_ADJUST_DEFAULT
                | TOKEN_ADJUST_SESSIONID,
            &mut own,
        )
    }
    .map_err(|e| format!("OpenProcessToken: {e}"))?;
    let own = Owned(own);
    let mut copy = HANDLE::default();
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
    .map_err(|e| format!("DuplicateTokenEx: {e}"))?;
    let copy = Owned(copy);
    unsafe {
        SetTokenInformation(
            copy.0,
            TokenSessionId,
            (&raw const session).cast(),
            size_of::<u32>() as u32,
        )
    }
    .map_err(|e| format!("SetTokenInformation(session {session}): {e}"))?;
    Ok(copy)
}

/// What the desktop helper keeps of SYSTEM's privileges: none of note (it reads and writes the screen through what
/// the desktops' own rights give SYSTEM).
pub const HELPER_PRIVILEGES: &[&str] = &["SeChangeNotifyPrivilege"];

/// What the agent for the sign-in screen keeps of LocalService's: restarting the PC (a viewer may from the sign-in
/// screen) and named objects; not SeImpersonatePrivilege or SeAssignPrimaryTokenPrivilege, the known ways from
/// LocalService back to SYSTEM.
pub const LOGON_AGENT_PRIVILEGES: &[&str] = &[
    "SeChangeNotifyPrivilege",
    "SeShutdownPrivilege",
    "SeCreateGlobalPrivilege",
];

/// A copy of `token` (in the same session) with every privilege taken out but those named in `keep`: a privilege
/// taken out cannot be enabled again by the process (the security review of 2026-11-22: the desktop helper ran with
/// all of SYSTEM's, SeDebugPrivilege and SeTcbPrivilege among them, and the agent for the sign-in screen with
/// LocalService's SeImpersonatePrivilege).
pub fn without_privileges(token: &Owned, keep: &[&str]) -> Result<Owned, String> {
    let delete: Vec<LUID_AND_ATTRIBUTES> = queqiao_core::platform::token_privileges(token.0)
        .into_iter()
        .filter(|(_, name)| !keep.contains(&name.as_str()))
        .map(|(luid, _)| LUID_AND_ATTRIBUTES {
            Luid: luid,
            ..Default::default()
        })
        .collect();
    let mut restricted = HANDLE::default();
    unsafe {
        CreateRestrictedToken(
            token.0,
            CREATE_RESTRICTED_TOKEN_FLAGS(0),
            None,
            (!delete.is_empty()).then_some(delete.as_slice()),
            None,
            &mut restricted,
        )
    }
    .map_err(|e| format!("CreateRestrictedToken: {e}"))?;
    let restricted = Owned(restricted);
    // The copy keeps the session; set again should it not have.
    if let (Some(wanted), Some(got)) = (session_of(token), session_of(&restricted))
        && wanted != got
    {
        unsafe {
            SetTokenInformation(
                restricted.0,
                TokenSessionId,
                (&raw const wanted).cast(),
                size_of::<u32>() as u32,
            )
        }
        .map_err(|e| format!("SetTokenInformation(session {wanted}): {e}"))?;
    }
    Ok(restricted)
}

fn session_of(token: &Owned) -> Option<u32> {
    let mut session = 0u32;
    let mut size = 0u32;
    unsafe {
        GetTokenInformation(
            token.0,
            TokenSessionId,
            Some((&raw mut session).cast()),
            size_of::<u32>() as u32,
            &mut size,
        )
    }
    .ok()
    .map(|()| session)
}

/// A copy of the person's token at High integrity when it is below (an administrator's full token is High already):
/// the person's other programs (Medium) can then not hook the agent, send its windows messages, open its threads or
/// write into it (the security review of 2026-11-22: they could, and so reach its channel to the helper, which
/// works the secure desktops). What the agent starts runs at Medium again (queqiao_core::platform::create_process).
pub fn raised(token: &Owned) -> Result<Owned, String> {
    let mut copy = HANDLE::default();
    unsafe {
        DuplicateTokenEx(
            token.0,
            TOKEN_ALL_ACCESS,
            None,
            SecurityImpersonation,
            TokenPrimary,
            &mut copy,
        )
    }
    .map_err(|e| format!("DuplicateTokenEx: {e}"))?;
    let copy = Owned(copy);
    let high = queqiao_core::platform::HIGH_INTEGRITY;
    if queqiao_core::platform::integrity_level(copy.0).is_some_and(|level| level < high) {
        queqiao_core::platform::set_integrity_level(copy.0, high)
            .map_err(|e| format!("the agent's integrity level: {e}"))?;
    }
    Ok(copy)
}

/// A token of LocalService (no rights of note, no profile secrets of anyone): the agent for the sign-in screen
/// runs as it, in session 0.
pub fn local_service_token() -> Result<Owned, String> {
    let mut token = HANDLE::default();
    unsafe {
        LogonUserW(
            windows::core::w!("LocalService"),
            windows::core::w!("NT AUTHORITY"),
            PCWSTR::null(),
            LOGON32_LOGON_SERVICE,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    }
    .map_err(|e| format!("LogonUserW(LocalService): {e}"))?;
    Ok(Owned(token))
}

/// The token of the person signed in to `session`; an error if no one is.
pub fn user_token(session: u32) -> Result<Owned, String> {
    let mut token = HANDLE::default();
    unsafe { WTSQueryUserToken(session, &mut token) }
        .map_err(|e| format!("no one is signed in to session {session} ({e})"))?;
    Ok(Owned(token))
}

/// The token's user, as a SID string (S-1-5-21-...).
pub fn user_sid(token: &Owned) -> Result<String, String> {
    let mut size = 0u32;
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut size) };
    // u64s: TOKEN_USER holds pointers.
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut size,
        )
    }
    .map_err(|e| format!("GetTokenInformation(TokenUser): {e}"))?;
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let mut text = PWSTR::null();
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
        .map_err(|e| format!("ConvertSidToStringSidW: {e}"))?;
    let sid = unsafe { text.to_string() }.map_err(|e| e.to_string());
    unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    sid
}

/// The token's user as "DOMAIN\user" (what LogonUserW takes).
pub fn user_account(token: &Owned) -> Result<String, String> {
    use windows::Win32::Security::{LookupAccountSidW, SID_NAME_USE};
    let mut size = 0u32;
    let _ = unsafe { GetTokenInformation(token.0, TokenUser, None, 0, &mut size) };
    let mut buffer = vec![0u64; (size as usize).div_ceil(8).max(1)];
    unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            (buffer.len() * 8) as u32,
            &mut size,
        )
    }
    .map_err(|e| format!("GetTokenInformation(TokenUser): {e}"))?;
    let user = unsafe { &*(buffer.as_ptr() as *const TOKEN_USER) };
    let (mut name, mut domain) = (vec![0u16; 256], vec![0u16; 256]);
    let (mut name_length, mut domain_length) = (name.len() as u32, domain.len() as u32);
    let mut kind = SID_NAME_USE::default();
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            user.User.Sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_length,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_length,
            &mut kind,
        )
    }
    .map_err(|e| format!("LookupAccountSidW: {e}"))?;
    Ok(format!(
        "{}\\{}",
        String::from_utf16_lossy(&domain[..domain_length as usize]),
        String::from_utf16_lossy(&name[..name_length as usize])
    ))
}

/// Everything started for one console session; `end` (or dropping it) ends all of it.
pub struct Job(Owned);

impl Job {
    /// A job that ends everything in it when it is closed.
    pub fn new() -> Result<Job, String> {
        Job::make(None)
    }

    /// The same, and its process may start no other: the desktop helper's and the sign-in agent's (neither needs to,
    /// so one that was taken over cannot start a program either).
    pub fn single() -> Result<Job, String> {
        Job::make(Some(1))
    }

    fn make(processes: Option<u32>) -> Result<Job, String> {
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| format!("CreateJobObjectW: {e}"))?;
        let job = Job(Owned(job));
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if let Some(processes) = processes {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_ACTIVE_PROCESS;
            limits.BasicLimitInformation.ActiveProcessLimit = processes;
        }
        unsafe {
            SetInformationJobObject(
                job.0.0,
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .map_err(|e| format!("SetInformationJobObject: {e}"))?;
        Ok(job)
    }

    pub fn end(&self) {
        let _ = unsafe { TerminateJobObject(self.0.0, 1) };
    }
}

pub struct Child {
    pub process: Owned,
    pub pid: u32,
}

impl Child {
    /// The exit code once it has ended.
    pub fn exit_code(&self) -> Option<u32> {
        match unsafe { WaitForSingleObject(self.process.0, 0) } {
            WAIT_OBJECT_0 => {
                let mut code = 0u32;
                unsafe { GetExitCodeProcess(self.process.0, &mut code) }
                    .ok()
                    .map(|()| code)
            }
            WAIT_TIMEOUT => None,
            _ => Some(u32::MAX),
        }
    }

    pub fn end(&self) {
        let _ = unsafe { TerminateProcess(self.process.0, 1) };
    }
}

/// A security descriptor from SDDL, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn new(sddl: &str) -> Result<Descriptor, String> {
        let text = wide(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(text.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(|e| format!("security descriptor {sddl}: {e}"))?;
        Ok(Descriptor(descriptor))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

/// The interactive desktop of a session.
pub const INTERACTIVE: &str = r"winsta0\default";
/// A window station and desktop of the token's own logon session, made for it (nothing on the screen).
pub const OWN_WINDOW_STATION: &str = "";

/// Starts `program arguments` with `token` on `desktop`, inheriting only `inherit`, in `job`. With
/// `protect_from` (the user's SID), that user may only wait for the process and end it.
pub fn start(
    token: &Owned,
    program: &Path,
    arguments: &str,
    inherit: &[HANDLE],
    protect_from: Option<&str>,
    desktop: &str,
    job: &Job,
) -> Result<Child, String> {
    let mut command = wide(&format!("\"{}\" {arguments}", program.display()));
    let folder = wide(&program.parent().unwrap_or(program).display().to_string());
    let desktop = wide(desktop);

    let mut size = 0usize;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut size) };
    let mut memory = vec![0u64; size.div_ceil(8).max(1)];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(memory.as_mut_ptr().cast());
    unsafe { InitializeProcThreadAttributeList(Some(list), 1, None, &mut size) }
        .map_err(|e| format!("InitializeProcThreadAttributeList: {e}"))?;
    struct ListGuard(LPPROC_THREAD_ATTRIBUTE_LIST);
    impl Drop for ListGuard {
        fn drop(&mut self) {
            unsafe { DeleteProcThreadAttributeList(self.0) };
        }
    }
    let _list = ListGuard(list);
    if !inherit.is_empty() {
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                HANDLE_LIST,
                Some(inherit.as_ptr().cast()),
                std::mem::size_of_val(inherit),
                None,
                None,
            )
        }
        .map_err(|e| format!("UpdateProcThreadAttribute(handle list): {e}"))?;
    }
    let mut info = STARTUPINFOEXW::default();
    info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    info.StartupInfo.lpDesktop = PWSTR(desktop.as_ptr() as *mut u16);
    info.lpAttributeList = list;

    // Process: SYSTEM all; the user (and the owner, whose implicit rights this replaces) only
    // PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE. Thread: SYNCHRONIZE |
    // THREAD_QUERY_LIMITED_INFORMATION.
    let descriptors = match protect_from {
        Some(sid) => Some((
            Descriptor::new(&format!(
                "D:P(A;;GA;;;SY)(A;;0x00101001;;;{sid})(A;;0x00101001;;;OW)"
            ))?,
            Descriptor::new(&format!(
                "D:P(A;;GA;;;SY)(A;;0x00100800;;;{sid})(A;;0x00100800;;;OW)"
            ))?,
        )),
        None => None,
    };
    let attributes = descriptors
        .as_ref()
        .map(|(process, thread)| (process.attributes(), thread.attributes()));

    let mut environment: *mut core::ffi::c_void = std::ptr::null_mut();
    unsafe { CreateEnvironmentBlock(&mut environment, Some(token.0), false) }
        .map_err(|e| format!("CreateEnvironmentBlock: {e}"))?;
    let mut process = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessAsUserW(
            Some(token.0),
            PCWSTR::null(),
            Some(PWSTR(command.as_mut_ptr())),
            attributes
                .as_ref()
                .map(|(p, _)| p as *const SECURITY_ATTRIBUTES),
            attributes
                .as_ref()
                .map(|(_, t)| t as *const SECURITY_ATTRIBUTES),
            !inherit.is_empty(),
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_UNICODE_ENVIRONMENT
                // No console (the service's own program is a console one): a console host would be a second
                // process in a job of one.
                | DETACHED_PROCESS
                | CREATE_SUSPENDED,
            Some(environment.cast_const()),
            PCWSTR(folder.as_ptr()),
            &info.StartupInfo,
            &mut process,
        )
    };
    let _ = unsafe { DestroyEnvironmentBlock(environment) };
    created.map_err(|e| format!("CreateProcessAsUserW {}: {e}", program.display()))?;
    let child = Child {
        process: Owned(process.hProcess),
        pid: process.dwProcessId,
    };
    let thread = Owned(process.hThread);
    if let Err(error) = unsafe { AssignProcessToJobObject(job.0.0, child.process.0) } {
        child.end();
        return Err(format!("AssignProcessToJobObject: {error}"));
    }
    unsafe { ResumeThread(thread.0) };
    Ok(child)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn own_token() -> Owned {
        let mut token = HANDLE::default();
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY | TOKEN_ASSIGN_PRIMARY,
                &mut token,
            )
        }
        .unwrap();
        Owned(token)
    }

    fn names(token: &Owned) -> Vec<String> {
        queqiao_core::platform::token_privileges(token.0)
            .into_iter()
            .map(|(_, name)| name)
            .collect()
    }

    #[test]
    fn privileges_are_taken_out_but_the_kept_ones() {
        let own = own_token();
        let before = names(&own);
        assert!(
            before.iter().any(|name| name == "SeChangeNotifyPrivilege"),
            "{before:?}"
        );
        let restricted =
            without_privileges(&own, &["SeChangeNotifyPrivilege", "SeTcbPrivilege"]).unwrap();
        // Kept only what was there: naming one adds nothing.
        let expected: Vec<String> = before
            .iter()
            .filter(|name| *name == "SeChangeNotifyPrivilege" || *name == "SeTcbPrivilege")
            .cloned()
            .collect();
        assert_eq!(names(&restricted), expected, "before: {before:?}");
        assert_eq!(session_of(&restricted), session_of(&own));
    }

    #[test]
    fn a_job_of_one_process_starts_no_other() {
        let token = without_privileges(&own_token(), HELPER_PRIVILEGES).unwrap();
        let cmd =
            std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or("C:\\Windows".into()))
                .join("System32")
                .join("cmd.exe");
        let run = |job: &Job| {
            let child = start(
                &token,
                &cmd,
                "/d /c cmd.exe /d /c exit 5",
                &[],
                None,
                "",
                job,
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            loop {
                if let Some(code) = child.exit_code() {
                    return code;
                }
                assert!(Instant::now() < deadline, "cmd.exe did not end");
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        assert_eq!(
            run(&Job::new().unwrap()),
            5,
            "in an ordinary job the inner program runs"
        );
        assert_ne!(
            run(&Job::single().unwrap()),
            5,
            "in a job of one process it cannot start"
        );
    }
}
