//! Starting the desktop helper and the agents (docs/LINUX.md, Security). Each starts in a process group and session of
//! its own (ended with everything it started), dies with the service (PR_SET_PDEATHSIG), inherits nothing but its
//! channels' pipes, and gets an environment made here, never one taken whole from elsewhere:
//! - the desktop helper: root, with only CAP_SYS_ADMIN (a framebuffer's buffer, KMS) and CAP_DAC_OVERRIDE (another
//!   user's display, for the notice) left in its bounding set, and no new privileges;
//! - the person's agent: the person, with their groups, and their session's display, bus and runtime folder (names
//!   from a fixed list, read from the session's own processes);
//! - the agent for the sign-in screen: the system user `queqiao` (no display, files or rights of its own), no new
//!   privileges.

use std::ffi::{CStr, CString};
use std::os::fd::RawFd;
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child as Process, Command, Stdio};
use std::time::{Duration, Instant};

use super::logind::Session;

/// The system user the agent for the sign-in screen runs as (made by the package).
pub const SIGN_IN_USER: &str = "queqiao";

const CAP_DAC_OVERRIDE: libc::c_int = 1;
const CAP_SYS_ADMIN: libc::c_int = 21;
const HELPER_CAPABILITIES: [libc::c_int; 2] = [CAP_DAC_OVERRIDE, CAP_SYS_ADMIN];

/// A process the service started.
pub struct Child {
    pub pid: u32,
    process: Process,
}

impl Child {
    /// Its exit code once it ended (128 + the signal that ended it).
    pub fn exit_code(&mut self) -> Option<i32> {
        use std::os::unix::process::ExitStatusExt;
        let status = self.process.try_wait().ok()??;
        Some(
            status
                .code()
                .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        )
    }

    /// Ends it and everything in its process group: asked first (SIGTERM), made to after 3 s (SIGKILL).
    pub fn end(mut self) {
        let group = -(self.pid as libc::pid_t);
        unsafe { libc::kill(group, libc::SIGTERM) };
        let until = Instant::now() + Duration::from_secs(3);
        while Instant::now() < until {
            if self.exit_code().is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe { libc::kill(group, libc::SIGKILL) };
        let _ = self.process.wait();
    }
}

/// A local account, from the system's user database.
#[derive(Clone, Debug)]
pub struct Account {
    pub uid: u32,
    pub gid: u32,
    pub name: String,
    pub home: PathBuf,
    pub shell: String,
    pub groups: Vec<libc::gid_t>,
}

fn account(entry: &libc::passwd) -> Account {
    let field = |pointer: *const libc::c_char| {
        if pointer.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned()
        }
    };
    let name = field(entry.pw_name);
    let mut groups = vec![0 as libc::gid_t; 256];
    let mut count = groups.len() as libc::c_int;
    if let Ok(c_name) = CString::new(name.clone()) {
        let found = unsafe {
            libc::getgrouplist(
                c_name.as_ptr(),
                entry.pw_gid,
                groups.as_mut_ptr(),
                &mut count,
            )
        };
        groups.truncate(if found < 0 { 0 } else { count.max(0) as usize });
    }
    if !groups.contains(&entry.pw_gid) {
        groups.push(entry.pw_gid);
    }
    Account {
        uid: entry.pw_uid,
        gid: entry.pw_gid,
        name,
        home: PathBuf::from(field(entry.pw_dir)),
        shell: field(entry.pw_shell),
        groups,
    }
}

pub fn account_by_uid(uid: u32) -> Result<Account, String> {
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16384];
    let mut found = std::ptr::null_mut();
    unsafe {
        libc::getpwuid_r(
            uid,
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if found.is_null() {
        return Err(format!("no account with the number {uid}"));
    }
    Ok(account(&entry))
}

pub fn account_by_name(name: &str) -> Result<Account, String> {
    let c_name = CString::new(name).map_err(|_| format!("no account {name:?}"))?;
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16384];
    let mut found = std::ptr::null_mut();
    unsafe {
        libc::getpwnam_r(
            c_name.as_ptr(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if found.is_null() {
        return Err(format!("no account {name:?}"));
    }
    Ok(account(&entry))
}

/// As whom a process runs.
pub enum Identity {
    /// Root, reduced (the desktop helper).
    Helper,
    /// The person signed in: their numbers and groups, nothing of root's (what they start may still raise itself,
    /// as sudo in a command line does).
    Person(Account),
    /// The sign-in screen's system user, with no new privileges either.
    SignIn(Account),
}

/// Starts `program` with `arguments` as `identity`, with exactly `environment`, the pipes `inherit` open across exec
/// (at the same numbers), standard input from /dev/null and standard error into the service's (the journal).
pub fn start(
    program: &Path,
    arguments: &[String],
    identity: &Identity,
    environment: &[(String, String)],
    inherit: &[RawFd],
) -> Result<Child, String> {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .env_clear()
        .envs(environment.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    let user = match identity {
        Identity::Person(account) | Identity::SignIn(account) => {
            command.current_dir(if account.home.is_dir() {
                account.home.as_path()
            } else {
                Path::new("/")
            });
            Some((account.uid, account.gid, account.groups.clone()))
        }
        Identity::Helper => {
            command.current_dir("/");
            None
        }
    };
    let inherit = inherit.to_vec();
    let no_new_privileges = matches!(identity, Identity::SignIn(_));
    // After fork, before exec: system calls only (nothing that allocates or locks).
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            for &fd in &inherit {
                if libc::fcntl(fd, libc::F_SETFD, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            match &user {
                Some((uid, gid, groups)) => {
                    if libc::setgroups(groups.len(), groups.as_ptr()) < 0
                        || libc::setgid(*gid) < 0
                        || libc::setuid(*uid) < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    // Changing user clears this; set after.
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
                    if libc::getuid() == 0 || libc::geteuid() == 0 {
                        return Err(std::io::Error::from_raw_os_error(libc::EPERM));
                    }
                    if no_new_privileges && libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                None => {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
                    // Everything else out of the bounding set: what exec gives root afterwards.
                    for capability in 0..64 {
                        if HELPER_CAPABILITIES.contains(&capability) {
                            continue;
                        }
                        if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) < 0
                            && *libc::__errno_location() != libc::EINVAL
                        {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
            }
            Ok(())
        });
    }
    let process = command
        .spawn()
        .map_err(|e| format!("{}: {e}", program.display()))?;
    Ok(Child {
        pid: process.id(),
        process,
    })
}

/// What a session's own programs know that an agent needs: its display, bus and locale. Only these names are taken
/// from the session's processes (never LD_* or anything else they may carry).
const FROM_SESSION: &[&str] = &[
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_CURRENT_DESKTOP",
    "XDG_SESSION_DESKTOP",
    "DESKTOP_SESSION",
    "DBUS_SESSION_BUS_ADDRESS",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "LC_TIME",
    "LC_NUMERIC",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_DIRS",
    "GTK_IM_MODULE",
    "QT_IM_MODULE",
    "XMODIFIERS",
    "XCURSOR_THEME",
    "XCURSOR_SIZE",
];

fn read_environment(pid: u32) -> Option<Vec<(String, String)>> {
    let bytes = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    Some(
        bytes
            .split(|&b| b == 0)
            .filter_map(|entry| {
                let entry = std::str::from_utf8(entry).ok()?;
                let (name, value) = entry.split_once('=')?;
                Some((name.to_string(), value.to_string()))
            })
            .collect(),
    )
}

/// The environment for the person's agent in `session`: from their account and logind, and the display, bus and
/// locale as the session's own programs have them (one that names the session first; a program of the person's user
/// manager, which GNOME and KDE start their shells under, otherwise). Fails while the desktop is not up yet.
pub fn session_environment(
    session: &Session,
    account: &Account,
) -> Result<Vec<(String, String)>, String> {
    let runtime = format!("/run/user/{}", account.uid);
    let wanted = if session.kind == "wayland" {
        "WAYLAND_DISPLAY"
    } else {
        "DISPLAY"
    };
    let mut best: Option<(u8, Vec<(String, String)>)> = None;
    let entries = std::fs::read_dir("/proc").map_err(|e| format!("/proc: {e}"))?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if entry.metadata().map(|m| m.uid()).ok() != Some(account.uid) {
            continue;
        }
        let Some(environment) = read_environment(pid) else {
            continue;
        };
        let value = |name: &str| {
            environment
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        };
        if value(wanted).is_none_or(str::is_empty) {
            continue;
        }
        let score = match value("XDG_SESSION_ID") {
            Some(id) if id == session.id => 2,
            None => 1,
            Some(_) => continue,
        };
        if best.as_ref().is_none_or(|(kept, _)| score > *kept) {
            let taken = environment
                .into_iter()
                .filter(|(name, value)| {
                    FROM_SESSION.contains(&name.as_str())
                        && value.len() < 4096
                        && !value.contains('\0')
                })
                .collect();
            best = Some((score, taken));
        }
    }
    let mut environment = best.map(|(_, taken)| taken).ok_or_else(|| {
        format!(
            "session {}: its desktop is not up yet (no program of it has {wanted})",
            session.id
        )
    })?;
    let has = |environment: &Vec<(String, String)>, name: &str| {
        environment.iter().any(|(n, _)| n == name)
    };
    // The display must be there: the Wayland socket in the runtime folder, the X server's socket.
    if let Some((_, display)) = environment.iter().find(|(n, _)| n == wanted) {
        let present = if wanted == "WAYLAND_DISPLAY" {
            let socket = if display.starts_with('/') {
                PathBuf::from(display)
            } else {
                Path::new(&runtime).join(display)
            };
            socket.exists()
        } else {
            let number = display
                .trim_start_matches(|c: char| c != ':')
                .trim_start_matches(':');
            let number = number.split('.').next().unwrap_or("");
            display.starts_with(':') && Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
        };
        if !present {
            return Err(format!(
                "session {}: its display {display} is not there",
                session.id
            ));
        }
    }
    if session.kind == "x11" && !has(&environment, "DISPLAY") && !session.display.is_empty() {
        environment.push(("DISPLAY".into(), session.display.clone()));
    }
    if !has(&environment, "DBUS_SESSION_BUS_ADDRESS") && Path::new(&runtime).join("bus").exists() {
        environment.push((
            "DBUS_SESSION_BUS_ADDRESS".into(),
            format!("unix:path={runtime}/bus"),
        ));
    }
    environment.extend([
        ("HOME".to_string(), account.home.display().to_string()),
        ("USER".to_string(), account.name.clone()),
        ("LOGNAME".to_string(), account.name.clone()),
        ("SHELL".to_string(), account.shell.clone()),
        (
            "PATH".to_string(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        ),
        ("XDG_SESSION_ID".to_string(), session.id.clone()),
        ("XDG_SESSION_TYPE".to_string(), session.kind.clone()),
        ("XDG_SESSION_CLASS".to_string(), "user".to_string()),
        ("XDG_SEAT".to_string(), "seat0".to_string()),
    ]);
    if Path::new(&runtime).is_dir() {
        environment.push(("XDG_RUNTIME_DIR".into(), runtime));
    }
    Ok(environment)
}

/// The environment for the agent for the sign-in screen and the helper: nothing of anyone's session.
pub fn plain_environment(account: Option<&Account>) -> Vec<(String, String)> {
    let mut environment = vec![
        (
            "PATH".to_string(),
            "/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
        ),
        (
            "LANG".to_string(),
            std::env::var("LANG").unwrap_or_else(|_| "C.UTF-8".into()),
        ),
    ];
    if let Some(account) = account {
        environment.extend([
            ("HOME".to_string(), account.home.display().to_string()),
            ("USER".to_string(), account.name.clone()),
            ("LOGNAME".to_string(), account.name.clone()),
        ]);
    }
    environment
}
