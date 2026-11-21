//! Updates of an installed Queqiao: the agent that runs from `%LOCALAPPDATA%\Programs\Queqiao` (put there by
//! queqiao-setup) looks at the update feed two minutes after it starts and then once a day (or when the person asks),
//! on a thread of its own. A later version, signed by the release key, is downloaded into
//! `%LOCALAPPDATA%\Queqiao\updates` and checked (queqiao_update); the main loop then starts its installer with
//! `--quiet --update` between sessions and leaves. The installer puts the earlier version back if the new agent does
//! not keep running.
//!
//! A version that was tried and did not keep running (the installer put this one back) is not tried again: the agent
//! notes the version it installs (`UpdateTried` in `HKCU\Software\Queqiao`) and skips it while it is still older.
//!
//! Tests point it elsewhere in the person's registry (`HKCU\Software\Queqiao`: `UpdateFeed`, a feed address, HTTPS or
//! HTTP on this PC only; `UpdateFirstCheckSeconds`): whatever the feed, only what the release key signed is installed.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_DWORD,
    RRF_RT_REG_SZ, RegCloseKey, RegCreateKeyExW, RegGetValueW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

const DAY: Duration = Duration::from_secs(24 * 3600);
/// After a failed check (no network, the feed unreachable), the next one.
const AFTER_FAILURE: Duration = Duration::from_secs(3600);

pub enum Found {
    UpToDate {
        by_hand: bool,
    },
    Ready {
        version: String,
        notes: String,
        installer: PathBuf,
        by_hand: bool,
    },
    Failed {
        error: String,
        by_hand: bool,
    },
    /// This version was tried here and did not keep running.
    Skipped {
        version: String,
        by_hand: bool,
    },
}

pub struct Updater {
    requests: Sender<()>,
    results: Receiver<Found>,
}

impl Updater {
    pub fn start(lines: &mut Vec<String>) -> Updater {
        let (requests, wanted) = mpsc::channel::<()>();
        let (found, results) = mpsc::channel();
        let feed = feed();
        let first = Duration::from_secs(u64::from(
            read_number("UpdateFirstCheckSeconds").unwrap_or(120),
        ));
        lines.push(format!(
            "updates: from {feed}, first look in {} s",
            first.as_secs()
        ));
        let _ = std::thread::Builder::new()
            .name("queqiao-update".into())
            .spawn(move || {
                let mut wait = first;
                loop {
                    let by_hand = match wanted.recv_timeout(wait) {
                        Ok(()) => true,
                        Err(RecvTimeoutError::Timeout) => false,
                        Err(RecvTimeoutError::Disconnected) => return,
                    };
                    let result = look(&feed, by_hand);
                    wait = if matches!(result, Found::Failed { .. }) {
                        AFTER_FAILURE
                    } else {
                        DAY
                    };
                    if found.send(result).is_err() {
                        return;
                    }
                }
            });
        Updater { requests, results }
    }

    /// Looks now (the person chose "Check for updates...").
    pub fn request(&self) {
        let _ = self.requests.send(());
    }

    pub fn try_result(&self) -> Option<Found> {
        self.results.try_recv().ok()
    }
}

fn look(feed: &str, by_hand: bool) -> Found {
    match queqiao_update::check(feed, queqiao_core::VERSION) {
        Ok(None) => Found::UpToDate { by_hand },
        Ok(Some(manifest)) if failed_before(&manifest.version) => Found::Skipped {
            version: manifest.version,
            by_hand,
        },
        Ok(Some(manifest)) => match queqiao_update::download(feed, &manifest, &folder()) {
            Ok(installer) => Found::Ready {
                version: manifest.version,
                notes: manifest.notes,
                installer,
                by_hand,
            },
            Err(error) => Found::Failed { error, by_hand },
        },
        Err(error) => Found::Failed { error, by_hand },
    }
}

/// Where downloaded installers go.
fn folder() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("Queqiao")
        .join("updates")
}

/// Whether this agent runs from an installation by queqiao-setup (only those update themselves).
pub fn installed_here() -> bool {
    let (Some(local), Ok(me)) = (std::env::var_os("LOCALAPPDATA"), std::env::current_exe()) else {
        return false;
    };
    let folder = PathBuf::from(local).join("Programs").join("Queqiao");
    let prefix = folder.display().to_string().to_ascii_lowercase() + "\\";
    me.display()
        .to_string()
        .to_ascii_lowercase()
        .starts_with(&prefix)
}

/// Whether `version` was installed here before and this older version is still the one running (it was put back).
fn failed_before(version: &str) -> bool {
    read_text("UpdateTried").is_some_and(|tried| {
        tried == version && queqiao_update::newer(&tried, queqiao_core::VERSION)
    })
}

/// Starts the downloaded installer for the update to `version` (it ends this agent if it still runs, installs,
/// starts the new one, and puts this version back if that does not keep running).
pub fn apply(installer: &Path, version: &str) -> Result<(), String> {
    write_text("UpdateTried", version)?;
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let report = folder().join("update_report.txt");
    std::process::Command::new(installer)
        .args(["--quiet", "--update", "--report"])
        .arg(&report)
        .current_dir(installer.parent().unwrap_or(installer))
        .creation_flags(DETACHED_PROCESS)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", installer.display()))
}

fn feed() -> String {
    read_text("UpdateFeed")
        .filter(|feed| queqiao_update::check_feed(feed).is_ok())
        .unwrap_or_else(|| queqiao_update::FEED.to_string())
}

fn read_text(name: &str) -> Option<String> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Queqiao"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    let length = (size as usize / 2).saturating_sub(1).min(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..length]))
}

fn write_text(name: &str, value: &str) -> Result<(), String> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    let failed = |e: windows::core::Error| format!("HKCU\\Software\\Queqiao: {}", e.message());
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Queqiao"),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    }
    .ok()
    .map_err(failed)?;
    let result = unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&data)) };
    let _ = unsafe { RegCloseKey(key) };
    result.ok().map_err(failed)
}

fn read_number(name: &str) -> Option<u32> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Queqiao"),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    }
    .ok()
    .ok()?;
    Some(value)
}
