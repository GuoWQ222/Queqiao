//! Installing Queqiao for the person using this PC: the programs in `%LOCALAPPDATA%\Programs\Queqiao`, a Start menu
//! shortcut to the device list (the viewer), an entry under Settings > Apps (to uninstall), and the Queqiao service
//! for the sign-in, lock and UAC screens, which also starts the agent at each sign-in (Windows asks for an
//! administrator once; the service copies its programs to `%ProgramFiles%\Queqiao`). The service is not a choice
//! (the user, 2026-11-27); only test builds install without it (`--no-service`, the agent at sign-in through the Run
//! key if asked). An earlier installation is kept as `Queqiao.old` until the new one is in place, and put
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

use crate::display_driver;
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
/// queqiao: links (help invitations) open the device list's help form.
const LINK_KEY: &str = r"Software\Classes\queqiao";

pub struct Choices {
    /// Where the programs go (the person's choice in the window; [`target`] for an update).
    pub folder: PathBuf,
    pub autostart: bool,
    pub service: bool,
    /// A shortcut on the desktop, and one in the Start menu.
    pub desktop: bool,
    pub start_menu: bool,
}

/// What an update keeps: its folder, start with Windows if the Run key starts this installation's agent, the
/// shortcuts there are; the service as it is.
pub fn current_choices() -> Choices {
    let folder = target();
    let agent = folder
        .join(PROGRAMS[1])
        .display()
        .to_string()
        .to_ascii_lowercase();
    Choices {
        autostart: read_value(RUN_KEY, RUN_VALUE)
            .is_some_and(|command| command.to_ascii_lowercase().contains(&agent)),
        service: false,
        desktop: desktop_shortcut().is_some_and(|link| link.is_file()),
        // An installation from before the choice always had it.
        start_menu: shortcut().is_file() || !folder.join(SETUP).is_file(),
        folder,
    }
}

/// Whether Queqiao is installed for this person (where [`target`] says), and its version.
pub fn installed() -> Option<String> {
    target()
        .join(SETUP)
        .is_file()
        .then(|| read_value(UNINSTALL_KEY, "DisplayVersion").unwrap_or_else(|| "?".into()))
}

/// How much room the programs take (the files this installer carries, and itself).
pub fn required_bytes(entries: &[Entry]) -> u64 {
    let me = std::env::current_exe()
        .ok()
        .and_then(|me| std::fs::metadata(me).ok())
        .map_or(0, |meta| meta.len());
    entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .sum::<u64>()
        + me
}

/// The folder typed or chosen in the window, checked: a folder on a fixed drive of this PC (not a whole drive, not a
/// network or removable one), that this person may write to without an administrator, with room for `required`
/// bytes, and empty or an earlier installation (an uninstall removes what it put there; a folder holding other files
/// is refused so nothing of the person's could be mixed in). Queqiao already installed elsewhere stays there. The
/// folder as it will be used, or why not (in the person's language).
pub fn check_folder(text: &str, required: u64) -> Result<PathBuf, String> {
    use queqiao_core::text::{tr, trf};
    let text = text.trim().trim_matches('"').trim().replace('/', "\\");
    let text = text.trim_end_matches('\\');
    if text.is_empty() {
        return Err(tr("Choose a folder for Queqiao.").into());
    }
    let bytes = text.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if !has_drive || bytes.get(2).is_some_and(|byte| *byte != b'\\') {
        return Err(tr("Choose a folder on a drive of this PC, such as C:\\Queqiao.").into());
    }
    if text.len() <= 2 {
        return Err(tr("Choose a folder, not a whole drive.").into());
    }
    if text.contains("\\..") || text.contains("\\.\\") || text.contains(['*', '?', '<', '>', '|']) {
        return Err(tr("That is not a folder name Windows can use.").into());
    }
    let folder = PathBuf::from(text);
    let root = format!("{}\\", &text[..2]);
    if drive_type(&root) != Some(DRIVE_FIXED) {
        return Err(tr(
            "Queqiao must go on a fixed drive of this PC, not a network or removable one.",
        )
        .into());
    }
    let current = target();
    if current.join(SETUP).is_file() && !same_folder(&current, &folder) {
        return Err(trf(
            "Queqiao is installed in {folder}. To install it elsewhere, uninstall it first.",
            &[("folder", &current.display().to_string())],
        ));
    }
    if folder.exists() {
        if !folder.is_dir() {
            return Err(tr("A file has that name. Choose a folder.").into());
        }
        let ours = folder.join(SETUP).is_file();
        let empty = std::fs::read_dir(&folder).is_ok_and(|mut files| files.next().is_none());
        if !ours && !empty {
            return Err(
                tr("This folder holds other files. Choose an empty folder or a new one.").into(),
            );
        }
        // Made by someone else (another person of this PC could make C:\Queqiao first and, as its owner, change its
        // permissions back after ours and swap the programs): only a folder of this person's, SYSTEM's or the
        // administrators' (security review of 2026-11-27).
        if !owned_by_this_person(&folder) {
            return Err(tr(
                "This folder belongs to another user of this PC. Choose a folder of your own, such as the one suggested.",
            )
            .into());
        }
    }
    if !writable(&folder) {
        return Err(tr(
            "You cannot write to this folder without an administrator. Choose a folder of your own, such as the one suggested.",
        )
        .into());
    }
    if let Some(free) = free_bytes(&folder)
        && free < required + required / 4
    {
        return Err(trf(
            "There is not enough room on {drive}: {needed} needed, {free} free.",
            &[
                ("drive", &text[..2]),
                ("needed", &size_text(required)),
                ("free", &size_text(free)),
            ],
        ));
    }
    Ok(folder)
}

/// "26 MB", "1.4 GB".
pub fn size_text(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{} MB", mb.ceil() as u64)
    }
}

/// The free room for `folder` (on its drive), if Windows says.
pub fn free_bytes(folder: &Path) -> Option<u64> {
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let existing = folder.ancestors().find(|path| path.exists())?;
    let path = wide(&existing.display().to_string());
    let mut free = 0u64;
    unsafe { GetDiskFreeSpaceExW(PCWSTR(path.as_ptr()), Some(&mut free), None, None) }.ok()?;
    Some(free)
}

const DRIVE_FIXED: u32 = 3;

fn drive_type(root: &str) -> Option<u32> {
    use windows::Win32::Storage::FileSystem::GetDriveTypeW;
    let root = wide(root);
    Some(unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) })
}

fn same_folder(a: &Path, b: &Path) -> bool {
    let name = |path: &Path| {
        path.display()
            .to_string()
            .trim_end_matches('\\')
            .to_ascii_lowercase()
    };
    name(a) == name(b)
}

/// Whether this person may make `folder` (or write in it): a folder of a test made and removed where it would go.
fn writable(folder: &Path) -> bool {
    let Some(existing) = folder.ancestors().find(|path| path.is_dir()) else {
        return false;
    };
    let probe = existing.join(format!("queqiao-setup-check-{}", std::process::id()));
    let made = std::fs::create_dir(&probe).is_ok();
    if made {
        let _ = std::fs::remove_dir(&probe);
    }
    made
}

/// Whether `folder`'s owner is this person, SYSTEM or the Administrators group (not another person of this PC).
fn owned_by_this_person(folder: &Path) -> bool {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
    };
    use windows::Win32::Security::{OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};
    let Some(me) = this_person_sid() else {
        return false;
    };
    let path = wide(&folder.display().to_string());
    unsafe {
        let mut owner = PSID::default();
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let read = GetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&raw mut owner),
            None,
            None,
            None,
            &mut descriptor,
        );
        if read.0 != 0 {
            return false;
        }
        let mut text = PWSTR::null();
        let owner_text = ConvertSidToStringSidW(owner, &mut text)
            .ok()
            .and_then(|()| text.to_string().ok());
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        owner_text
            .is_some_and(|owner| owner == me || owner == "S-1-5-18" || owner == "S-1-5-32-544")
    }
}

/// This person's SID as text ("S-1-5-21-...").
fn this_person_sid() -> Option<String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut buffer = vec![0u8; 256];
        let mut length = 0u32;
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            &mut length,
        );
        let _ = CloseHandle(token);
        got.ok()?;
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text).ok()?;
        let sid = text.to_string().ok();
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
        sid
    }
}

/// Only this person, SYSTEM and the administrators may change the programs (a folder chosen outside the person's own
/// would otherwise take the place's permissions: under C:\ every signed-in user may change new files, and another user
/// of this PC could replace the agent that starts at this person's sign-in).
fn protect_folder(folder: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)
            .map_err(|e| format!("permissions: {}", e.message()))?;
        let mut buffer = vec![0u8; 256];
        let mut length = 0u32;
        let got = GetTokenInformation(
            token,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            buffer.len() as u32,
            &mut length,
        );
        let _ = CloseHandle(token);
        got.map_err(|e| format!("permissions: {}", e.message()))?;
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid_text = PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut sid_text)
            .map_err(|e| format!("permissions: {}", e.message()))?;
        let sid = sid_text.to_string().unwrap_or_default();
        let _ = LocalFree(Some(HLOCAL(sid_text.0.cast())));
        // Full control for SYSTEM, the administrators and this person, inherited by everything inside; nothing else.
        let sddl = wide(&format!(
            "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{sid})"
        ));
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut descriptor,
            None,
        )
        .map_err(|e| format!("permissions: {}", e.message()))?;
        let (mut present, mut defaulted) = (false.into(), false.into());
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let read = GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut defaulted);
        let path = wide(&folder.display().to_string());
        let set = read.map_err(|e| e.message()).and_then(|()| {
            SetNamedSecurityInfoW(
                PCWSTR(path.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                None,
                None,
                Some(dacl),
                None,
            )
            .ok()
            .map_err(|e| e.message())
        });
        let _ = LocalFree(Some(HLOCAL(descriptor.0)));
        set.map_err(|e| format!("permissions of {}: {e}", folder.display()))
    }
}

/// The desktop's Queqiao shortcut (the person's desktop folder, wherever Windows keeps it).
fn desktop_shortcut() -> Option<PathBuf> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::{FOLDERID_Desktop, KNOWN_FOLDER_FLAG, SHGetKnownFolderPath};
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Desktop, KNOWN_FOLDER_FLAG(0), None).ok()?;
        let folder = path.to_string().ok();
        CoTaskMemFree(Some(path.0.cast()));
        folder.map(|folder| PathBuf::from(folder).join("Queqiao.lnk"))
    }
}

/// The earlier installation's place while the new one goes in ("Queqiao" -> "Queqiao.old").
fn old_folder(target: &Path) -> PathBuf {
    let name = target.file_name().map_or_else(
        || "Queqiao".into(),
        |name| name.to_string_lossy().into_owned(),
    );
    target.with_file_name(format!("{name}.old"))
}

/// What an installation puts in its folder (removed again by an uninstall, nothing else).
fn installed_files(entries: &[Entry]) -> Vec<String> {
    let mut names: Vec<String> = PROGRAMS.iter().map(|name| name.to_string()).collect();
    names.push(SETUP.into());
    names.extend(["LICENSE".to_string(), "THIRD_PARTY_NOTICES.md".to_string()]);
    for entry in entries {
        if !names.contains(&entry.name) {
            names.push(entry.name.clone());
        }
    }
    names
}

/// A step of the install for the window: how far (0 to 100) and what it does now (in the person's language).
pub type Progress<'a> = &'a mut dyn FnMut(u32, String);

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

/// Where this person's Queqiao is (the folder chosen when it was installed), or goes by default:
/// `%LOCALAPPDATA%\Programs\Queqiao`.
pub fn target() -> PathBuf {
    queqiao_core::platform::installation_folder()
}

fn shortcut() -> PathBuf {
    known("APPDATA", r"C:\Users\Public\AppData\Roaming")
        .join(r"Microsoft\Windows\Start Menu\Programs\Queqiao.lnk")
}

/// `Program Files\Queqiao`, the machine's folder from the registry: never from environment variables, which an
/// elevated program inherits from the person, and any program of theirs can change (HKCU\Environment).
pub(crate) fn program_files() -> PathBuf {
    queqiao_core::platform::program_files().join("Queqiao")
}

pub fn install(
    entries: &[Entry],
    choices: &Choices,
    lines: &mut Vec<String>,
    progress: Progress,
) -> Result<(), String> {
    use queqiao_core::text::tr;
    for program in PROGRAMS {
        if !entries.iter().any(|entry| entry.name == program) {
            return Err(format!("this installer does not carry {program}"));
        }
    }
    let target = choices.folder.clone();
    let installed_at = self::target();
    if installed_at.join(SETUP).is_file() && !same_folder(&installed_at, &target) {
        return Err(format!(
            "Queqiao is installed in {}: uninstall it first to install it elsewhere",
            installed_at.display()
        ));
    }
    progress(2, tr("Closing Queqiao...").into());
    stop_running(&target, lines);
    let old = old_folder(&target);
    if old.exists() {
        // Only what an earlier install left there (never a folder of the person's that has the name).
        if !old.join(SETUP).is_file() {
            return Err(format!(
                "{} is in the way (it is not an earlier Queqiao): move or remove it",
                old.display()
            ));
        }
        std::fs::remove_dir_all(&old).map_err(|e| format!("{}: {e}", old.display()))?;
    }
    let had_earlier = target.join(SETUP).is_file();
    if had_earlier {
        std::fs::rename(&target, &old)
            .map_err(|e| format!("keeping the earlier installation: {e}"))?;
    } else if target.exists() {
        // An empty folder the person chose (check_folder): Queqiao's own one replaces it.
        std::fs::remove_dir(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    if let Err(error) = place(entries, &target, progress) {
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

    // The service comes first: the agent started next leaves for the service's own. This installer itself runs as an
    // administrator for it (install_service), never a program in the person's own folder. Not again when the service
    // already runs this installer's programs (the same installer run twice; or the service put in by an
    // administrator first, as on a test PC with nobody at the screen to answer Windows' prompt).
    if choices.service && service_is_current(entries) {
        lines.push("the Queqiao service has this version already".into());
        // The service's step installs the display driver (Super Screen) too; without that step, on its own when this
        // installer's driver is not in place. Not having it never stops the install.
        if display_driver::carried(entries) && !display_driver::is_current(entries) {
            progress(66, tr("Installing the display driver...").into());
            let me = std::env::current_exe().map_err(|e| e.to_string())?;
            match run_elevated(&me, "--install-display-driver") {
                Ok(_) => lines.extend(display_driver::report_lines()),
                Err(error) => lines.push(format!(
                    "display driver: not installed ({error}): Super Screen is unavailable"
                )),
            }
        }
    } else if choices.service {
        progress(64, tr("Installing the Queqiao service...").into());
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        let failed = match run_elevated(&me, "--install-service") {
            Ok(0) => {
                lines.push(
                    "the Queqiao service is installed (sign-in, lock and UAC screens)".into(),
                );
                lines.extend(display_driver::report_lines());
                None
            }
            Ok(code) => Some(format!("installing the service failed (exit code {code})")),
            Err(error) => Some(format!("the service was not installed: {error}")),
        };
        // Nothing half done (the administrator's prompt declined, 2026-11-23: new programs were left beside the
        // earlier service, the earlier ones in Queqiao.old): the earlier version back, or none at all.
        if let Some(error) = failed {
            return Err(undo(&target, &old, had_earlier, &error, lines));
        }
    }

    // Queqiao for this person now. The new agent must keep running: an update goes back to the earlier version if
    // it does not.
    let agent = target.join(PROGRAMS[1]);
    let started = start_and_watch(&agent, &mut |part| {
        progress(70 + (part * 20.0) as u32, tr("Starting Queqiao...").into())
    });
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

    progress(92, tr("Adding Queqiao to Windows...").into());
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

    // The shortcuts open Queqiao's main window: the agent's (the one running already shows its own).
    progress(96, tr("Making shortcuts...").into());
    if choices.start_menu {
        make_shortcut(&shortcut(), &target.join(PROGRAMS[1]), "--show")?;
        lines.push(format!("Start menu: {}", shortcut().display()));
    } else {
        let _ = std::fs::remove_file(shortcut());
    }
    if let Some(link) = desktop_shortcut() {
        if choices.desktop {
            make_shortcut(&link, &target.join(PROGRAMS[1]), "--show")?;
            lines.push(format!("desktop: {}", link.display()));
        } else {
            let _ = std::fs::remove_file(&link);
        }
    }

    // Help invitation links: only fill in the help form (connecting is the person's click).
    let viewer = target.join(PROGRAMS[0]).display().to_string();
    set_value(LINK_KEY, "", Value::Text("URL:Queqiao"))?;
    set_value(LINK_KEY, "URL Protocol", Value::Text(""))?;
    set_value(
        &format!(r"{LINK_KEY}\DefaultIcon"),
        "",
        Value::Owned(viewer.clone()),
    )?;
    set_value(
        &format!(r"{LINK_KEY}\shell\open\command"),
        "",
        Value::Owned(format!("\"{viewer}\" --invite \"%1\"")),
    )?;
    lines.push("queqiao: links open the help form".into());

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
    progress(100, tr("Done.").into());
    started.map(|_| ())
}

/// Starts the agent and watches it for [`START_WATCH`]: still running then, or ended on purpose, is a start.
/// `watched` hears how much of the watch has gone (0 to 1).
fn start_and_watch(agent: &Path, watched: &mut dyn FnMut(f64)) -> Result<String, String> {
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
            Ok(None) => {
                let left = deadline.saturating_duration_since(Instant::now());
                watched(1.0 - left.as_secs_f64() / START_WATCH.as_secs_f64());
                std::thread::sleep(Duration::from_millis(200))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok("the agent is running (tray icon)".into())
}

/// The earlier installation back in place and its agent started; the error to report.
fn roll_back(target: &Path, old: &Path, error: &str, lines: &mut Vec<String>) -> String {
    match put_back(target, old, lines) {
        Ok(()) => format!("the update did not keep running ({error}): the earlier version is back"),
        Err(problem) => format!(
            "the update did not keep running ({error}), and the earlier version could not be put back: {problem}"
        ),
    }
}

/// After a step failed once the new programs were placed: the earlier installation back and its agent started
/// again (`had_earlier`), or the new programs removed (a first installation). The error, for the report.
fn undo(
    target: &Path,
    old: &Path,
    had_earlier: bool,
    error: &str,
    lines: &mut Vec<String>,
) -> String {
    if had_earlier {
        return match put_back(target, old, lines) {
            Ok(()) => format!("{error}: the earlier version is back"),
            Err(problem) => {
                format!("{error}, and the earlier version could not be put back: {problem}")
            }
        };
    }
    stop_running(target, lines);
    remove_folder(target);
    if target.exists() {
        return format!("{error}, and {} could not be removed", target.display());
    }
    format!("{error}: nothing was installed")
}

/// The new programs removed, the earlier ones (`old`) back in their place, their agent started again.
fn put_back(target: &Path, old: &Path, lines: &mut Vec<String>) -> Result<(), String> {
    stop_running(target, lines);
    remove_folder(target);
    std::fs::rename(old, target).map_err(|problem| problem.to_string())?;
    lines.push("the earlier version is back".into());
    match start_and_watch(&target.join(PROGRAMS[1]), &mut |_| {}) {
        Ok(note) => lines.push(format!("earlier version: {note}")),
        Err(problem) => lines.push(format!(
            "the earlier agent did not keep running either: {problem}"
        )),
    }
    Ok(())
}

/// Removes `folder`, trying again for 10 s (a program that was just stopped may still hold its files).
fn remove_folder(folder: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while folder.exists() && std::fs::remove_dir_all(folder).is_err() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn place(entries: &[Entry], target: &Path, progress: Progress) -> Result<(), String> {
    use queqiao_core::text::trf;
    // The display driver's files are only for the administrator's step, which takes them from this installer itself.
    let entries: Vec<&Entry> = entries
        .iter()
        .filter(|entry| !display_driver::FILES.contains(&entry.name.as_str()))
        .collect();
    std::fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    // Before anything goes in: the programs inherit the folder's permissions.
    protect_folder(target)?;
    let total = entries
        .iter()
        .map(|entry| entry.bytes.len() as u64)
        .sum::<u64>()
        .max(1);
    let mut done = 0u64;
    for entry in entries {
        progress(
            5 + (done * 55 / total) as u32,
            trf("Copying {file}...", &[("file", &entry.name)]),
        );
        let path = target.join(&entry.name);
        std::fs::write(&path, &entry.bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        done += entry.bytes.len() as u64;
    }
    // This installer stays with the programs (to uninstall); not when it already runs from there.
    progress(60, trf("Copying {file}...", &[("file", SETUP)]));
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

/// Whether the Queqiao service is installed with exactly this installer's service and agent (SHA-256 of the files in
/// Program Files, which only administrators may change).
fn service_is_current(entries: &[Entry]) -> bool {
    use sha2::{Digest, Sha256};
    service_installed()
        && [PROGRAMS[2], PROGRAMS[1]].iter().all(|program| {
            let Some(entry) = entries.iter().find(|entry| entry.name == *program) else {
                return false;
            };
            std::fs::read(program_files().join(program))
                .is_ok_and(|installed| Sha256::digest(&installed) == Sha256::digest(&entry.bytes))
        })
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
    } else if display_driver::present(&crate::payload::own().unwrap_or_default()) {
        // The display driver without the service (`--remove-service` takes it otherwise): reported, not a failure.
        let me = std::env::current_exe().map_err(|e| e.to_string())?;
        lines.push(match run_elevated(&me, "--remove-display-driver") {
            Ok(0) => "the display driver and its certificate are removed".into(),
            Ok(code) => format!("the display driver was not removed completely (exit code {code})"),
            Err(error) => format!("the display driver was not removed: {error}"),
        });
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
    if let Some(link) = desktop_shortcut() {
        let _ = std::fs::remove_file(link);
    }
    delete_tree(UNINSTALL_KEY)?;
    delete_tree(LINK_KEY)?;
    // Downloaded updates.
    let updates = known("LOCALAPPDATA", r"C:\Users\Public\AppData\Local").join(r"Queqiao\updates");
    if updates.exists() && std::fs::remove_dir_all(&updates).is_ok() {
        lines.push(format!("{} is removed", updates.display()));
    }
    lines.push("the Start menu shortcut and the Apps entry are removed".into());
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    // Only what the install put there (the folder may be one the person chose; anything else of theirs stays).
    let names = installed_files(&crate::payload::own().unwrap_or_default());
    // A program that just ended may hold its file a moment longer.
    let deadline = Instant::now() + Duration::from_secs(5);
    let left = loop {
        let mut left = Vec::new();
        for name in &names {
            let path = target.join(name);
            if !path.exists() || same_file(&path, &me) {
                continue;
            }
            if std::fs::remove_file(&path).is_err() {
                left.push(path.display().to_string());
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
    // This installer may run from the folder itself: it goes, then the folder if nothing else is in it, once it has
    // ended.
    if me.starts_with(&target) {
        let command = format!(
            "/c ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
            me.display(),
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
    // Installers the service kept for its updates.
    let updates = queqiao_core::platform::program_data().join(r"Queqiao\updates");
    if updates.exists() && std::fs::remove_dir_all(&updates).is_ok() {
        lines.push(format!("{} is removed", updates.display()));
    }
    // The display driver and its certificate, once nothing of the service uses them.
    if let Err(error) = display_driver::remove(&crate::payload::own().unwrap_or_default(), lines) {
        lines.push(format!("display driver: {error}"));
    }
    Ok(())
}

/// The processes that run from `folder` (pid, program path), except this one.
pub(crate) fn processes_from(folder: &Path) -> Vec<(u32, String)> {
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };
    let prefix = folder.display().to_string().to_ascii_lowercase() + "\\";
    let own = std::process::id();
    let mut found = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
    while more {
        let pid = entry.th32ProcessID;
        if pid != own
            && let Ok(process) =
                unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
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
            let _ = unsafe { CloseHandle(process) };
            if let Some(path) = path.filter(|path| path.to_ascii_lowercase().starts_with(&prefix)) {
                found.push((pid, path));
            }
        }
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }
    let _ = unsafe { CloseHandle(snapshot) };
    found
}

/// Ends the programs that run from `folder` (an earlier installation), except this one.
fn stop_running(folder: &Path, lines: &mut Vec<String>) {
    for (pid, path) in processes_from(folder) {
        let Ok(process) =
            (unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) })
        else {
            lines.push(format!("could not end {path} (process {pid})"));
            continue;
        };
        let ended = unsafe { TerminateProcess(process, 0) }.is_ok()
            && unsafe { WaitForSingleObject(process, 5000) } == WAIT_OBJECT_0;
        let _ = unsafe { CloseHandle(process) };
        lines.push(if ended {
            format!("ended {path} (process {pid})")
        } else {
            format!("could not end {path} (process {pid})")
        });
    }
}

/// The Queqiao service, as an administrator (`--install-service`, which the install starts through UAC). The service
/// and its agent come from this installer's own files (payload: from its mapped image, as Windows checked it), are
/// written to a new folder in Program Files (only administrators may make one there), and `queqiao-service install`
/// runs from it, putting them in `Program Files\Queqiao`. Nothing is taken from the person's own installation, which
/// any program of theirs may change: the security review of 2026-11-22 found that the administrator's step ran
/// `queqiao-service.exe` from there, and copied whatever lay beside it into Program Files as the SYSTEM service.
pub fn install_service(entries: &[Entry], lines: &mut Vec<String>) -> Result<(), String> {
    use std::io::Write;
    let staging = program_files().with_file_name("Queqiao.install");
    if let Ok(meta) = std::fs::symlink_metadata(&staging) {
        let removed = if meta.is_dir() && !meta.file_type().is_symlink() {
            std::fs::remove_dir_all(&staging)
        } else if meta.is_dir() {
            std::fs::remove_dir(&staging)
        } else {
            std::fs::remove_file(&staging)
        };
        removed.map_err(|e| format!("{}: {e}", staging.display()))?;
    }
    std::fs::create_dir(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
    let result = (|| {
        for program in [PROGRAMS[2], PROGRAMS[1]] {
            let entry = entries
                .iter()
                .find(|entry| entry.name == program)
                .ok_or_else(|| format!("this installer does not carry {program}"))?;
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(staging.join(program))
                .and_then(|mut file| file.write_all(&entry.bytes))
                .map_err(|e| format!("{program}: {e}"))?;
        }
        let output = std::process::Command::new(staging.join(PROGRAMS[2]))
            .arg("install")
            .current_dir(&staging)
            .creation_flags(CREATE_NO_WINDOW.0)
            .output()
            .map_err(|e| format!("{}: {e}", PROGRAMS[2]))?;
        for text in [&output.stdout, &output.stderr] {
            lines.extend(
                String::from_utf8_lossy(text)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(str::to_string),
            );
        }
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "queqiao-service install: exit code {:?}",
                output.status.code()
            ))
        }
    })();
    let _ = std::fs::remove_dir_all(&staging);
    // The display driver (Super Screen) once the service is in; without it Queqiao works, so its failure is only
    // reported (display_driver::report_lines for the person's install).
    if result.is_ok() {
        let _ = display_driver::install_reported(entries, lines);
    }
    result
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

fn make_shortcut(link: &Path, program: &Path, arguments: &str) -> Result<(), String> {
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
        let arguments = wide(arguments);
        shell
            .SetArguments(PCWSTR(arguments.as_ptr()))
            .map_err(|e| e.message())?;
        let description = wide("Queqiao");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder this person made is theirs; C:\Windows (TrustedInstaller's) and a folder another user owns are not,
    /// and check_folder refuses them (security review of 2026-11-27).
    #[test]
    fn a_folder_of_another_owner_is_refused() {
        let mine = std::env::temp_dir().join(format!("queqiao-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&mine);
        std::fs::create_dir_all(&mine).unwrap();
        assert!(this_person_sid().is_some_and(|sid| sid.starts_with("S-1-5-")));
        assert!(owned_by_this_person(&mine));
        let _ = std::fs::remove_dir_all(&mine);
        let windows =
            std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or(r"C:\Windows".into()));
        assert!(!owned_by_this_person(&windows));
        assert!(!owned_by_this_person(&windows.join("no-such-folder-here")));
        let refused = check_folder(&windows.display().to_string(), 1).unwrap_err();
        assert!(!refused.is_empty());
    }

    /// A folder of its own under %TEMP% with `files` (name, content) in `name`.
    fn folder(root: &Path, name: &str, files: &[(&str, &str)]) -> PathBuf {
        let path = root.join(name);
        std::fs::create_dir_all(&path).unwrap();
        for (file, content) in files {
            std::fs::write(path.join(file), content).unwrap();
        }
        path
    }

    fn scratch(test: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("queqiao_undo_{test}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn an_update_whose_service_step_failed_puts_the_earlier_version_back() {
        let root = scratch("update");
        let target = folder(&root, "Queqiao", &[("queqiao-viewer.exe", "new")]);
        let old = folder(&root, "Queqiao.old", &[("queqiao-viewer.exe", "earlier")]);
        let mut lines = Vec::new();
        let error = undo(
            &target,
            &old,
            true,
            "the service was not installed: cancelled",
            &mut lines,
        );
        assert_eq!(
            error,
            "the service was not installed: cancelled: the earlier version is back"
        );
        assert_eq!(
            std::fs::read_to_string(target.join("queqiao-viewer.exe")).unwrap(),
            "earlier"
        );
        assert!(!old.exists(), "Queqiao.old is left");
        assert!(
            lines
                .iter()
                .any(|line| line == "the earlier version is back")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_first_installation_whose_service_step_failed_leaves_nothing() {
        let root = scratch("first");
        let target = folder(&root, "Queqiao", &[("queqiao-viewer.exe", "new")]);
        let old = root.join("Queqiao.old");
        let mut lines = Vec::new();
        let error = undo(
            &target,
            &old,
            false,
            "installing the service failed (exit code 1)",
            &mut lines,
        );
        assert_eq!(
            error,
            "installing the service failed (exit code 1): nothing was installed"
        );
        assert!(!target.exists(), "the new programs are left");
        let _ = std::fs::remove_dir_all(&root);
    }
}
