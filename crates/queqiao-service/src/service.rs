//! The service: what Windows' service manager sees (start, stop, session changes), and the supervisor that keeps
//! the desktop helper and the agent running in the console session while someone is signed in there.
//!
//! The two are started together, with a fresh channel between them, and end together: when either ends, the
//! session at the screen changes, or the agent asks to be restarted. The agent's exit code says whether to start
//! it again: 0 (the person quit it) and 3 (not logged in to an account) wait for the next sign-in; anything else is
//! a failure, tried again after 1, 2, 4 ... 60 s.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

use queqiao_local::{FromService, ToService, Writer};

use crate::logon;
use windows::Win32::Foundation::{
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE, NO_ERROR,
    WAIT_OBJECT_0,
};
use windows::Win32::Security::Authentication::Identity::SendSAS;
use windows::Win32::System::Registry::{
    HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::RemoteDesktop::WTSGetActiveConsoleSessionId;
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::core::{PCWSTR, PWSTR};

use crate::launch::{self, Child, Job, Owned, wide};
use crate::log;

/// Where the service keeps its settings (written by `install`, administrators only).
pub const PARAMETERS: &str = r"SYSTEM\CurrentControlSet\Services\Queqiao\Parameters";

/// The agent's exit codes that mean "do not start me again until the next sign-in".
const AGENT_QUIT: u32 = 0;
const AGENT_NOT_LOGGED_IN: u32 = 3;

/// No session at the screen (WTSGetActiveConsoleSessionId while it changes).
const NO_SESSION: u32 = u32::MAX;

struct Shared {
    status: SERVICE_STATUS_HANDLE,
    stop: Owned,
    wake: Owned,
}

// Handles: usable from the handler's thread and the service's.
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

static SHARED: Mutex<Option<Shared>> = Mutex::new(None);

/// Runs as the service; `Ok(false)` when Windows did not start this program as one (started by hand).
pub fn dispatch() -> Result<bool, String> {
    let mut name = wide(crate::NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    match unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } {
        Ok(()) => Ok(true),
        Err(error) if error.code() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.to_hresult() => {
            Ok(false)
        }
        Err(error) => Err(format!("StartServiceCtrlDispatcherW: {}", error.message())),
    }
}

fn report(state: SERVICE_STATUS_CURRENT_STATE) {
    let Ok(shared) = SHARED.lock() else { return };
    let Some(shared) = shared.as_ref() else {
        return;
    };
    let accepted = if state == SERVICE_RUNNING {
        SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
    } else {
        0
    };
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: accepted,
        dwWin32ExitCode: NO_ERROR.0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: if state == SERVICE_RUNNING || state == SERVICE_STOPPED {
            0
        } else {
            10_000
        },
    };
    let _ = unsafe { SetServiceStatus(shared.status, &status) };
}

fn signal(which: impl Fn(&Shared) -> HANDLE) {
    if let Ok(shared) = SHARED.lock()
        && let Some(shared) = shared.as_ref()
    {
        let _ = unsafe { SetEvent(which(shared)) };
    }
}

unsafe extern "system" fn handler(
    control: u32,
    _event: u32,
    _data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            report(SERVICE_STOP_PENDING);
            signal(|shared| shared.stop.0);
            NO_ERROR.0
        }
        SERVICE_CONTROL_SESSIONCHANGE => {
            signal(|shared| shared.wake.0);
            NO_ERROR.0
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

unsafe extern "system" fn service_main(_count: u32, _arguments: *mut PWSTR) {
    let name = wide(crate::NAME);
    let Ok(status) =
        (unsafe { RegisterServiceCtrlHandlerExW(PCWSTR(name.as_ptr()), Some(handler), None) })
    else {
        return;
    };
    let events = unsafe {
        (
            CreateEventW(None, true, false, PCWSTR::null()),
            CreateEventW(None, false, false, PCWSTR::null()),
        )
    };
    let (Ok(stop), Ok(wake)) = events else {
        return;
    };
    let (stop, wake) = (Owned(stop), Owned(wake));
    let (stop_handle, wake_handle) = (stop.0, wake.0);
    if let Ok(mut shared) = SHARED.lock() {
        *shared = Some(Shared { status, stop, wake });
    }
    report(SERVICE_START_PENDING);
    report(SERVICE_RUNNING);
    log::line(&format!(
        "service started ({})",
        queqiao_core::banner(env!("CARGO_PKG_NAME"))
    ));
    supervise(stop_handle, wake_handle);
    log::line("service stopped");
    report(SERVICE_STOPPED);
}

/// The service's settings, read again before every start.
struct Settings {
    /// Added to the agent's command line (tests: a test server and account; normally empty: the saved login).
    agent_arguments: String,
    /// Start the agent again after a failure (tests turn this off, so a test agent runs once).
    restart_agent: bool,
    /// Added to the command line of the agent for the sign-in screen (tests: how long, a report).
    logon_arguments: String,
}

fn read_string(key: &str, value: &str) -> Option<String> {
    let (key, value) = (wide(key), wide(value));
    let mut buffer = vec![0u16; 4096];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let length = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(
        &buffer[..length.min(buffer.len())],
    ))
}

pub fn read_dword(key: &str, value: &str) -> Option<u32> {
    let (key, value) = (wide(key), wide(value));
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut data).cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    Some(data)
}

impl Settings {
    fn read() -> Settings {
        Settings {
            agent_arguments: read_string(PARAMETERS, "AgentArguments").unwrap_or_default(),
            restart_agent: read_dword(PARAMETERS, "RestartAgent").is_none_or(|v| v != 0),
            logon_arguments: read_string(PARAMETERS, "LogonArguments").unwrap_or_default(),
        }
    }
}

/// The helper and the agent of one console session.
struct Pair {
    id: u64,
    session: u32,
    /// The agent for the sign-in screen (nobody signed in), not the person's.
    logon: bool,
    /// A job each: every process of a job must be in the job's session, and the agent for the sign-in screen
    /// (session 0) was refused the helper's (the console session's) with "access denied".
    helper_job: Job,
    agent_job: Job,
    helper: Child,
    agent: Child,
    to_agent: Writer,
    started: Instant,
}

impl Pair {
    fn end(self, reason: &str) {
        self.agent_job.end();
        self.helper_job.end();
        log::line(&format!(
            "session {}: helper {} and agent {} ended: {reason}",
            self.session, self.helper.pid, self.agent.pid
        ));
    }
}

fn folder() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

fn start_pair(
    id: u64,
    session: u32,
    settings: &Settings,
    requests: &Sender<(u64, ToService)>,
    wake: HANDLE,
) -> Result<Pair, String> {
    let folder = folder();
    // The person signed in at the screen, as them; nobody signed in: the agent for the sign-in screen as
    // LocalService in session 0 (on a window station of its own: nothing it shows is seen), when the person allowed
    // it (the device was handed over).
    let (token, sid, logon_identity) = match launch::user_token(session) {
        Ok(user) => {
            let sid = launch::user_sid(&user)?;
            (user, sid, None)
        }
        Err(nobody) => {
            if !logon::stored() {
                return Err(format!(
                    "{nobody}; control at the sign-in screen is not set up"
                ));
            }
            let identity = logon::load().ok_or(
                "the device for the sign-in screen cannot be read (made anew at the next sign-in)",
            )?;
            (
                launch::local_service_token()?,
                "S-1-5-19".to_string(),
                Some(identity),
            )
        }
    };
    let logon = logon_identity.is_some();
    let (helper_job, agent_job) = (Job::new()?, Job::new()?);
    let (helper_ends, agent_desktop_ends) = queqiao_local::channel()?;
    let system = launch::system_token(session)?;
    let helper = launch::start(
        &system,
        &folder.join("queqiao-service.exe"),
        &format!("desktop --channel {}", helper_ends.argument()),
        &helper_ends.for_child()?,
        None,
        launch::INTERACTIVE,
        &helper_job,
    )?;
    drop(helper_ends);
    let (ours, theirs) = queqiao_local::channel()?;
    let mut inherit = theirs.for_child()?.to_vec();
    inherit.extend(agent_desktop_ends.for_child()?);
    let arguments = if logon {
        format!(
            "--logon {} --service-channel {} --desktop-channel {}",
            settings.logon_arguments,
            theirs.argument(),
            agent_desktop_ends.argument()
        )
    } else {
        format!(
            "{} --service-channel {} --desktop-channel {}",
            settings.agent_arguments,
            theirs.argument(),
            agent_desktop_ends.argument()
        )
    };
    let desktop = if logon {
        launch::OWN_WINDOW_STATION
    } else {
        launch::INTERACTIVE
    };
    let agent = match launch::start(
        &token,
        &folder.join("queqiao-agent.exe"),
        arguments.trim_start(),
        &inherit,
        Some(&sid),
        desktop,
        &agent_job,
    ) {
        Ok(agent) => agent,
        Err(error) => {
            helper_job.end();
            return Err(error);
        }
    };
    drop((theirs, agent_desktop_ends));
    let (mut from_agent, mut to_agent) = ours.split();
    // The agent for the sign-in screen learns who it is first.
    if let Some(identity) = logon_identity
        && let Err(error) = to_agent.write(&FromService::Identity(identity).encode())
    {
        agent_job.end();
        helper_job.end();
        return Err(format!("handing over the device: {error}"));
    }
    let requests = requests.clone();
    let wake = wake.0 as usize;
    std::thread::spawn(move || {
        while let Ok(message) = from_agent.read() {
            match ToService::decode(&message) {
                Some(request) => {
                    if requests.send((id, request)).is_err() {
                        break;
                    }
                    let _ = unsafe { SetEvent(HANDLE(wake as *mut core::ffi::c_void)) };
                }
                None => {
                    log::line("the agent sent a request the service does not know: ignored");
                }
            }
        }
    });
    log::line(&format!(
        "session {session}: started the desktop helper ({}) and {} ({}) as {sid}",
        helper.pid,
        if logon {
            "the agent for the sign-in screen"
        } else {
            "the agent"
        },
        agent.pid
    ));
    Ok(Pair {
        id,
        session,
        logon,
        helper_job,
        agent_job,
        helper,
        agent,
        to_agent,
        started: Instant::now(),
    })
}

fn supervise(stop: HANDLE, wake: HANDLE) {
    let (requests, incoming) = mpsc::channel::<(u64, ToService)>();
    let mut pair: Option<Pair> = None;
    let mut next_id = 1u64;
    let mut failures = 0u32;
    let mut next_start = Instant::now();
    // The session whose agent left by itself (quit, not logged in, or a test run over): not started again until
    // someone signs in anew.
    let mut done_in: Option<u32> = None;
    let mut last_error = String::new();
    loop {
        let console = unsafe { WTSGetActiveConsoleSessionId() };
        while let Ok((id, request)) = incoming.try_recv() {
            let Some(current) = pair.as_mut().filter(|p| p.id == id) else {
                continue;
            };
            match request {
                ToService::SendSas => {
                    unsafe { SendSAS(false) };
                    log::line(&format!(
                        "session {}: Ctrl+Alt+Del sent for the agent",
                        current.session
                    ));
                    let reply = FromService::Reply("Ctrl+Alt+Del sent".into());
                    let _ = current.to_agent.write(&reply.encode());
                }
                ToService::Restart => {
                    if let Some(old) = pair.take() {
                        old.end("the agent asked to restart Queqiao");
                    }
                    failures = 0;
                    done_in = None;
                    next_start = Instant::now();
                }
                // Only the person's own agent hands the device over (or takes it back).
                ToService::LogonIdentity(_) | ToService::ForgetLogon if current.logon => {
                    log::line(
                        "the agent for the sign-in screen tried to change its device: ignored",
                    );
                }
                ToService::LogonIdentity(identity) => match logon::save(&identity) {
                    Ok(()) => log::line(&format!(
                        "session {}: the sign-in screen: device {} kept for it",
                        current.session, identity.device_id
                    )),
                    Err(error) => log::line(&format!("the sign-in screen: not kept: {error}")),
                },
                ToService::ForgetLogon => {
                    if logon::stored() {
                        logon::forget();
                        log::line(&format!(
                            "session {}: the sign-in screen: the device was forgotten (the person does not allow it)",
                            current.session
                        ));
                    }
                }
            }
        }

        if let Some(current) = &pair {
            let mut verdict: Option<(String, bool)> = None;
            if current.session != console {
                verdict = Some(("the session at the screen changed".into(), false));
            } else if current.logon && launch::user_token(console).is_ok() {
                // The person's own agent takes over.
                verdict = Some(("someone signed in".into(), false));
            } else if let Some(code) = current.agent.exit_code() {
                let finished = code == AGENT_QUIT || code == AGENT_NOT_LOGGED_IN;
                verdict = Some((format!("the agent left with code {code}"), !finished));
                if finished || !Settings::read().restart_agent {
                    done_in = Some(current.session);
                }
            } else if current.helper.exit_code().is_some() {
                verdict = Some(("the desktop helper ended".into(), true));
            }
            if let Some((reason, failed)) = verdict {
                let ran = current.started.elapsed();
                if let Some(old) = pair.take() {
                    old.end(&reason);
                }
                if failed {
                    failures = if ran > Duration::from_secs(60) {
                        1
                    } else {
                        failures + 1
                    };
                    let delay = Duration::from_secs(1 << failures.min(6).saturating_sub(1));
                    next_start = Instant::now() + delay.min(Duration::from_secs(60));
                }
            }
        }

        // A new session at the screen, or someone signed in after the agent left: start again there.
        if done_in.is_some_and(|session| session != console) {
            done_in = None;
        }
        if pair.is_none()
            && console != NO_SESSION
            && done_in != Some(console)
            && Instant::now() >= next_start
        {
            let id = next_id;
            next_id += 1;
            match start_pair(id, console, &Settings::read(), &requests, wake) {
                Ok(started) => {
                    pair = Some(started);
                    last_error.clear();
                }
                Err(error) => {
                    if error != last_error {
                        log::line(&format!("session {console}: not started: {error}"));
                        last_error = error;
                    }
                    next_start = Instant::now() + Duration::from_secs(2);
                }
            }
        }

        let mut handles = vec![stop, wake];
        if let Some(current) = &pair {
            handles.push(current.agent.process.0);
            handles.push(current.helper.process.0);
        }
        let result = unsafe { WaitForMultipleObjects(&handles, false, 2000) };
        if result == WAIT_OBJECT_0 {
            break;
        }
    }
    if let Some(old) = pair.take() {
        old.end("the service stops");
    }
}
