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
    DuplicateTokenEx, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    SecurityImpersonation, SetTokenInformation, TOKEN_ADJUST_DEFAULT, TOKEN_ADJUST_SESSIONID,
    TOKEN_ALL_ACCESS, TOKEN_ASSIGN_PRIMARY, TOKEN_DUPLICATE, TOKEN_QUERY, TOKEN_USER, TokenPrimary,
    TokenSessionId, TokenUser,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows::Win32::System::RemoteDesktop::WTSQueryUserToken;
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
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

/// Everything started for one console session; `end` (or dropping it) ends all of it.
pub struct Job(Owned);

impl Job {
    pub fn new() -> Result<Job, String> {
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| format!("CreateJobObjectW: {e}"))?;
        let job = Job(Owned(job));
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
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

/// Starts `program arguments` with `token` on the session's default desktop, inheriting only `inherit`, in
/// `job`. With `protect_from` (the user's SID), that user may only wait for the process and end it.
pub fn start(
    token: &Owned,
    program: &Path,
    arguments: &str,
    inherit: &[HANDLE],
    protect_from: Option<&str>,
    job: &Job,
) -> Result<Child, String> {
    let mut command = wide(&format!("\"{}\" {arguments}", program.display()));
    let folder = wide(&program.parent().unwrap_or(program).display().to_string());
    let desktop = wide(r"winsta0\default");

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
                | CREATE_NO_WINDOW
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
