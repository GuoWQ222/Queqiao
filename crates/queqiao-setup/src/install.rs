//! Installing Queqiao for the person using this PC, without an administrator: the programs in
//! `%LOCALAPPDATA%\Programs\Queqiao`, a Start menu shortcut to the device list (the viewer), an entry under Settings >
//! Apps (to uninstall), the agent at sign-in if chosen (the Run key), and if chosen the Queqiao service for the
//! sign-in, lock and UAC screens (Windows asks for an administrator once; the service copies its programs to
//! `%ProgramFiles%\Queqiao`). An earlier installation is kept as `Queqiao.old` until the new one is in place, and put
//! back if anything fails. Uninstalling takes all of it away again.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_FILE_NOT_FOUND, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW,
    RegDeleteKeyValueW, RegDeleteTreeW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, DETACHED_PROCESS, GetExitCodeProcess, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW, ShellLink,
};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{Interface, PCWSTR, PWSTR, w};

use crate::payload::Entry;

/// What an installation holds: the device list (viewer), the agent, the service, and this installer.
pub const PROGRAMS: [&str; 3] = [
    "queqiao-viewer.exe",
    "queqiao-agent.exe",
    "queqiao-service.exe",
];
pub const SETUP: &str = "queqiao-setup.exe";
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\Queqiao";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Queqiao";
const SERVICE_KEY: &str = r"SYSTEM\CurrentControlSet\Services\Queqiao";

pub struct Choices {
    pub autostart: bool,
    pub service: bool,
}

/// What an update keeps: start with Windows if the Run key starts this installation's agent; the service as it is.
pub fn current_choices() -> Choices {
    let agent = target()
        .join(PROGRAMS[1])
        .display()
        .to_string()
        .to_ascii_lowercase();
    Choices {
        autostart: read_value(RUN_KEY, RUN_VALUE)
            .is_some_and(|command| command.to_ascii_lowercase().contains(&agent)),
        service: false,
    }
}

/// How long a new agent must keep running to count as started.
const START_WATCH: Duration = Duration::from_secs(8);
/// The agent's exit codes that are not a failed start: 0 (it left: the Queqiao service runs Queqiao here) and 4
/// (another agent runs in this session).
const LEFT_ON_PURPOSE: [i32; 2] = [0, 4];

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

fn known(variable: &str, fallback: &str) -> PathBuf {
    std::env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(fallback))
}

/// Where the programs go: `%LOCALAPPDATA%\Programs\Queqiao`.
pub fn target() -> PathBuf {
    known("LOCALAPPDATA", r"C:\Users\Public\AppData\Local")
        .join("Programs")
        .join("Queqiao")
}

fn shortcut() -> PathBuf {
    known("APPDATA", r"C:\Users\Public\AppData\Roaming")
        .join(r"Microsoft\Windows\Start Menu\Programs\Queqiao.lnk")
}

fn program_files() -> PathBuf {
    known("ProgramFiles", r"C:\Program Files").join("Queqiao")
}

pub fn install(
    entries: &[Entry],
    choices: &Choices,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    for program in PROGRAMS {
        if !entries.iter().any(|entry| entry.name == program) {
            return Err(format!("this installer does not carry {program}"));
        }
    }
    let target = target();
    stop_running(&target, lines);
    let old = target.with_file_name("Queqiao.old");
    if old.exists() {
        std::fs::remove_dir_all(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    }
    let had_earlier = target.exists();
    if had_earlier {
        std::fs::rename(&target, &old)
            .map_err(|e| format!("keeping the earlier installation: {e}"))?;
    }
    if let Err(error) = place(entries, &target) {
        let _ = std::fs::remove_dir_all(&target);
        if had_earlier {
            let _ = std::fs::rename(&old, &target);
        }
        return Err(format!("{error} (the earlier installation is back)"));
    }
    lines.push(format!(
        "programs in {} ({} files)",
        target.display(),
        entries.len()
    ));

    // With the service chosen, it comes first: the agent started next leaves for the service's own.
    if choices.service {
        match run_elevated(&target.join(PROGRAMS[2]), "install") {
            Ok(0) => lines
                .push("the Queqiao service is installed (sign-in, lock and UAC screens)".into()),
            Ok(code) => return Err(format!("installing the service failed (exit code {code})")),
            Err(error) => return Err(format!("the service was not installed: {error}")),
        }
    }

    // Queqiao for this person now. The new agent must keep running: an update goes back to the earlier version if
    // it does not.
    let agent = target.join(PROGRAMS[1]);
    let started = start_and_watch(&agent);
    match &started {
        Ok(note) => lines.push(note.clone()),
        Err(error) if had_earlier => {
            lines.push(format!("the new agent did not keep running: {error}"));
            return Err(roll_back(&target, &old, error, lines));
        }
        Err(error) => lines.push(format!("the agent did not keep running: {error}")),
    }
    if had_earlier {
        let _ = std::fs::remove_dir_all(&old);
        lines.push("the earlier installation was replaced".into());
    }

    let setup = target.join(SETUP);
    let size_kb = entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .sum::<u64>()
        / 1024;
    let uninstall = format!("\"{}\" --uninstall", setup.display());
    let values: [(&str, Value); 9] = [
        ("DisplayName", Value::Text("Queqiao")),
        ("DisplayVersion", Value::Text(queqiao_core::VERSION)),
        ("Publisher", Value::Text("Queqiao")),
        (
            "DisplayIcon",
            Value::Owned(target.join(PROGRAMS[0]).display().to_string()),
        ),
        (
            "InstallLocation",
            Value::Owned(target.display().to_string()),
        ),
        ("UninstallString", Value::Owned(uninstall.clone())),
        (
            "QuietUninstallString",
            Value::Owned(format!("{uninstall} --quiet")),
        ),
        ("NoModify", Value::Number(1)),
        ("EstimatedSize", Value::Number(size_kb as u32)),
    ];
    for (name, value) in values {
        set_value(UNINSTALL_KEY, name, value)?;
    }
    set_value(UNINSTALL_KEY, "NoRepair", Value::Number(1))?;
    lines.push("listed under Settings > Apps (uninstall there)".into());

    make_shortcut(&shortcut(), &target.join(PROGRAMS[0]))?;
    lines.push(format!("Start menu: {}", shortcut().display()));

    let agent = target.join(PROGRAMS[1]);
    if choices.autostart {
        set_value(
            RUN_KEY,
            RUN_VALUE,
            Value::Owned(format!("\"{}\"", agent.display())),
        )?;
        lines.push("Queqiao starts when you sign in".into());
    } else {
        delete_value(RUN_KEY, RUN_VALUE)?;
    }

    // Installed, but Queqiao is not running: said, so it is not taken for a success.
    started.map(|_| ())
}

/// Starts the agent and watches it for [`START_WATCH`]: still running then, or ended on purpose, is a start.
fn start_and_watch(agent: &Path) -> Result<String, String> {
    let mut child = std::process::Command::new(agent)
        .current_dir(agent.parent().unwrap_or(agent))
        .creation_flags(DETACHED_PROCESS.0)
        .spawn()
        .map_err(|e| format!("it did not start: {e}"))?;
    let deadline = Instant::now() + START_WATCH;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                return match status.code() {
                    Some(code) if LEFT_ON_PURPOSE.contains(&code) => Ok(if code == 0 {
                        "the agent left by itself (exit code 0: the Queqiao service runs Queqiao here, or an update)"
                            .into()
                    } else {
                        "another Queqiao agent runs in this session".into()
                    }),
                    code => Err(format!(
                        "it ended within {} s (exit code {})",
                        START_WATCH.as_secs(),
                        code.map_or("none".into(), |code| code.to_string())
                    )),
                };
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok("the agent is running (tray icon)".into())
}

/// The earlier installation back in place and its agent started; the error to report.
fn roll_back(target: &Path, old: &Path, error: &str, lines: &mut Vec<String>) -> String {
    stop_running(target, lines);
    let deadline = Instant::now() + Duration::from_secs(10);
    while target.exists() && std::fs::remove_dir_all(target).is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
    }
    if let Err(problem) = std::fs::rename(old, target) {
        return format!(
            "the update did not keep running ({error}), and the earlier version could not be put back: {problem}"
        );
    }
    lines.push("the earlier version is back".into());
    match start_and_watch(&target.join(PROGRAMS[1])) {
        Ok(note) => lines.push(format!("earlier version: {note}")),
        Err(problem) => lines.push(format!(
            "the earlier agent did not keep running either: {problem}"
        )),
    }
    format!("the update did not keep running ({error}): the earlier version is back")
}

fn place(entries: &[Entry], target: &Path) -> Result<(), String> {
    std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    for entry in entries {
        let path = target.join(&entry.name);
        std::fs::write(&path, &entry.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    // This installer stays with the programs (to uninstall); not when it already runs from there.
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let setup = target.join(SETUP);
    if !same_file(&me, &setup) {
        std::fs::copy(&me, &setup).map_err(|e| format!("{}: {e}", setup.display()))?;
    }
    Ok(())
}

fn same_file(a: &Path, b: &Path) -> bool {
    std::fs::canonicalize(a)
        .ok()
        .zip(std::fs::canonicalize(b).ok())
        .is_some_and(|(a, b)| a == b)
}

/// Whether the Queqiao service is installed (its key is readable by everyone).
pub fn service_installed() -> bool {
    let key = wide(SERVICE_KEY);
    let mut handle = HKEY::default();
    let opened = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut handle,
        )
    };
    if opened.is_ok() {
        let _ = unsafe { RegCloseKey(handle) };
        true
    } else {
        false
    }
}

pub fn uninstall(lines: &mut Vec<String>) -> Result<(), String> {
    let target = target();
    if service_installed() {
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        match run_elevated(&me, "--remove-service") {
            Ok(0) => lines.push("the Queqiao service is removed".into()),
            Ok(code) => return Err(format!("removing the service failed (exit code {code})")),
            Err(error) => return Err(format!("the service was not removed: {error}")),
        }
    }
    stop_running(&target, lines);
    // The Run entry only if it starts this installation's agent.
    if read_value(RUN_KEY, RUN_VALUE).is_some_and(|command| {
        command
            .to_ascii_lowercase()
            .contains(&target.display().to_string().to_ascii_lowercase())
    }) {
        delete_value(RUN_KEY, RUN_VALUE)?;
    }
    let _ = std::fs::remove_file(shortcut());
    delete_tree(UNINSTALL_KEY)?;
    // Downloaded updates.
    let updates = known("LOCALAPPDATA", r"C:\Users\Public\AppData\Local").join(r"Queqiao\updates");
    if updates.exists() && std::fs::remove_dir_all(&updates).is_ok() {
        lines.push(format!("{} is removed", updates.display()));
    }
    lines.push("the Start menu shortcut and the Apps entry are removed".into());
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    // A program that just ended may hold its file a moment longer.
    let deadline = Instant::now() + Duration::from_secs(5);
    let left = loop {
        let mut left = Vec::new();
        if let Ok(files) = std::fs::read_dir(&target) {
            for file in files.flatten() {
                let path = file.path();
                if same_file(&path, &me) {
                    continue;
                }
                if std::fs::remove_file(&path).is_err() {
                    left.push(path.display().to_string());
                }
            }
        }
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    if !left.is_empty() {
        return Err(format!("still in use: {}", left.join(", ")));
    }
    // This installer may run from the folder itself: the folder goes once it has ended.
    if me.starts_with(&target) {
        let command = format!(
            "/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"",
            target.display()
        );
        let _ = std::process::Command::new("cmd.exe")
            .raw_arg(&command)
            .creation_flags((CREATE_NO_WINDOW | DETACHED_PROCESS).0)
            .spawn();
    } else {
        let _ = std::fs::remove_dir(&target);
    }
    lines.push(format!("{} is removed", target.display()));
    Ok(())
}

/// As an administrator (asked by Windows): the service and its programs in %ProgramFiles%\Queqiao.
pub fn remove_service(lines: &mut Vec<String>) -> Result<(), String> {
    let folder = program_files();
    let service = folder.join(PROGRAMS[2]);
    if service.is_file() {
        let output = std::process::Command::new(&service)
            .arg("uninstall")
            .output()
            .map_err(|e| format!("{}: {e}", service.display()))?;
        lines.extend(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::to_string),
        );
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
    }
    // The service's processes are gone once it stopped; its programs can go.
    let deadline = Instant::now() + Duration::from_secs(10);
    while folder.exists() && std::fs::remove_dir_all(&folder).is_err() && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(300));
    }
    if folder.exists() {
        return Err(format!("{} could not be removed", folder.display()));
    }
    lines.push(format!("{} is removed", folder.display()));
    Ok(())
}

/// Ends the programs that run from `folder` (an earlier installation), except this one.
fn stop_running(folder: &Path, lines: &mut Vec<String>) {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return;
    };
    let prefix = folder.display().to_string().to_ascii_lowercase() + "\\";
    let own = std::process::id();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let pid = entry.th32ProcessID;
        if pid != own
            && let Ok(process) = unsafe {
                OpenProcess(
                    PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                    false,
                    pid,
                )
            }
        {
            let mut name = [0u16; 1024];
            let mut length = name.len() as u32;
            let path = unsafe {
                QueryFullProcessImageNameW(
                    process,
                    PROCESS_NAME_WIN32,
                    PWSTR(name.as_mut_ptr()),
                    &mut length,
                )
            }
            .ok()
            .map(|()| String::from_utf16_lossy(&name[..length as usize]));
            if path
                .as_ref()
                .is_some_and(|path| path.to_ascii_lowercase().starts_with(&prefix))
            {
                let path = path.unwrap_or_default();
                let ended = unsafe { TerminateProcess(process, 0) }.is_ok()
                    && unsafe { WaitForSingleObject(process, 5000) } == WAIT_OBJECT_0;
                lines.push(if ended {
                    format!("ended {path} (process {pid})")
                } else {
                    format!("could not end {path} (process {pid})")
                });
            }
            let _ = unsafe { CloseHandle(process) };
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
}

/// Runs `program parameters` as an administrator: Windows asks (UAC); refusing it is an error. Its exit code.
fn run_elevated(program: &Path, parameters: &str) -> Result<u32, String> {
    let (file, parameters) = (wide(&program.display().to_string()), wide(parameters));
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: w!("runas"),
        lpFile: PCWSTR(file.as_ptr()),
        lpParameters: PCWSTR(parameters.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    unsafe { ShellExecuteExW(&mut info) }.map_err(|e| e.message())?;
    let process: HANDLE = info.hProcess;
    if process.is_invalid() {
        return Err("Windows did not start it".into());
    }
    let waited = unsafe { WaitForSingleObject(process, 120_000) };
    let mut code = u32::MAX;
    if waited == WAIT_OBJECT_0 {
        let _ = unsafe { GetExitCodeProcess(process, &mut code) };
    }
    let _ = unsafe { CloseHandle(process) };
    if waited != WAIT_OBJECT_0 {
        return Err("it did not finish within 2 minutes".into());
    }
    Ok(code)
}

fn make_shortcut(link: &Path, program: &Path) -> Result<(), String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let shell: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
            .map_err(|e| format!("shortcut: {}", e.message()))?;
        let (path, folder) = (
            wide(&program.display().to_string()),
            wide(&program.parent().unwrap_or(program).display().to_string()),
        );
        shell
            .SetPath(PCWSTR(path.as_ptr()))
            .map_err(|e| e.message())?;
        shell
            .SetWorkingDirectory(PCWSTR(folder.as_ptr()))
            .map_err(|e| e.message())?;
        let description = wide("Queqiao: your devices");
        let _ = shell.SetDescription(PCWSTR(description.as_ptr()));
        let file: IPersistFile = shell.cast().map_err(|e| e.message())?;
        if let Some(parent) = link.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let target = wide(&link.display().to_string());
        file.Save(PCWSTR(target.as_ptr()), true)
            .map_err(|e| format!("shortcut: {}", e.message()))
    }
}

enum Value<'a> {
    Text(&'a str),
    Owned(String),
    Number(u32),
}

fn set_value(key: &str, name: &str, value: Value) -> Result<(), String> {
    let (key_name, value_name) = (wide(key), wide(name));
    let mut handle = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
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
    let result = match value {
        Value::Number(number) => unsafe {
            RegSetValueExW(
                handle,
                PCWSTR(value_name.as_ptr()),
                None,
                REG_DWORD,
                Some(&number.to_le_bytes()),
            )
        },
        Value::Text(text) => set_text(handle, &value_name, text),
        Value::Owned(text) => set_text(handle, &value_name, &text),
    };
    let _ = unsafe { RegCloseKey(handle) };
    result
        .ok()
        .map_err(|e| format!("{key}\\{name}: {}", e.message()))
}

fn set_text(handle: HKEY, name: &[u16], text: &str) -> windows::Win32::Foundation::WIN32_ERROR {
    let data = wide(text);
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    unsafe { RegSetValueExW(handle, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) }
}

fn read_value(key: &str, name: &str) -> Option<String> {
    let (key, name) = (wide(key), wide(name));
    let mut buffer = vec![0u16; 2048];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
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

fn delete_value(key: &str, name: &str) -> Result<(), String> {
    let (key_name, value_name) = (wide(key), wide(name));
    let result = unsafe {
        RegDeleteKeyValueW(
            HKEY_CURRENT_USER,
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

fn delete_tree(key: &str) -> Result<(), String> {
    let key_name = wide(key);
    let result = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key_name.as_ptr())) };
    if result == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    result.ok().map_err(|e| format!("{key}: {}", e.message()))
}

/// For std::process::Command on Windows.
use std::os::windows::process::CommandExt;
