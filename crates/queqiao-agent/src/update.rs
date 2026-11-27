//! Updates of an installed Queqiao: the agent that runs from the folder queqiao-setup put it in (the person's choice,
//! `%LOCALAPPDATA%\Programs\Queqiao` by default) looks at the update feed two minutes after it starts and then once a day (or when the person asks),
//! on a thread of its own. A later version, signed by the release key, is downloaded into
//! `%LOCALAPPDATA%\Queqiao\updates` and checked (queqiao_update); the main loop then starts its installer with
//! `--quiet --update` between sessions and leaves. The installer puts the earlier version back if the new agent does
//! not keep running.
//!
//! A version that was tried and did not keep running (the installer put this one back) is not tried again: the agent
//! notes the version it installs in the person's own installation (`UpdateTried` in `HKCU\Software\Queqiao`) and
//! skips it while it is still older. With the Queqiao service the service's own note counts, for a week
//! (queqiao_update::RETRY_AFTER: then it is tried once more; `update_tried.txt` in
//! `%ProgramData%\Queqiao`: only SYSTEM and administrators may write it, the agent reads it), and the service checks
//! it again itself: a program of the person setting `UpdateTried` stops no update of the service (the security review
//! of 2026-11-22). The newest manifest seen (`UpdateNewestPublished`) keeps an
//! older feed served again from being taken.
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
        /// The manifest's exact bytes and signature, for the service (it checks them itself).
        manifest: Vec<u8>,
        signature: String,
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
    /// `through_service`: the Queqiao service updates its own copy (and decides what it tries again).
    pub fn start(lines: &mut Vec<String>, through_service: bool) -> Updater {
        let (requests, wanted) = mpsc::channel::<()>();
        let (found, results) = mpsc::channel();
        let feed = feed();
        // Tests look sooner (builds with the `test-switches` feature only).
        let first_check = if cfg!(feature = "test-switches") {
            read_number("UpdateFirstCheckSeconds")
        } else {
            None
        };
        let first = Duration::from_secs(u64::from(first_check.unwrap_or(120)));
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
                    let result = look(&feed, by_hand, through_service);
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

fn look(feed: &str, by_hand: bool, through_service: bool) -> Found {
    let newest_seen = read_text("UpdateNewestPublished")
        .and_then(|text| text.parse::<u64>().ok())
        .unwrap_or(0);
    let checked = queqiao_update::check(feed, queqiao_core::VERSION, newest_seen);
    if let Ok(checked) = &checked
        && checked.published > newest_seen
    {
        let _ = write_text("UpdateNewestPublished", &checked.published.to_string());
    }
    match checked.map(|checked| checked.offer) {
        Ok(None) => Found::UpToDate { by_hand },
        Ok(Some(offer)) if tried_before(&offer.manifest.version, through_service) => {
            Found::Skipped {
                version: offer.manifest.version,
                by_hand,
            }
        }
        Ok(Some(offer)) => match queqiao_update::download(feed, &offer.manifest, &folder()) {
            Ok(installer) => Found::Ready {
                version: offer.manifest.version,
                notes: offer.manifest.notes,
                installer,
                manifest: offer.raw,
                signature: offer.signature,
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
    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    // Where the person chose to install it (queqiao-setup records it), the default folder otherwise.
    let folder = queqiao_core::platform::installation_folder();
    let prefix = folder.display().to_string().to_ascii_lowercase() + "\\";
    me.display()
        .to_string()
        .to_ascii_lowercase()
        .starts_with(&prefix)
}

/// Whether `version` was tried and put back: by the service for its copy (its own note), or here.
fn tried_before(version: &str, through_service: bool) -> bool {
    if !through_service {
        return failed_before(version);
    }
    let note = queqiao_core::platform::program_data()
        .join("Queqiao")
        .join("update_tried.txt");
    std::fs::read_to_string(note).is_ok_and(|note| {
        queqiao_update::tried_recently(&note, version, queqiao_update::unix_now())
            && queqiao_update::newer(version, queqiao_core::VERSION)
    })
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

/// With the Queqiao service: this person's own installation (the device list in the Start menu) is updated first,
/// as above but waited for, then the service gets the update for its copy (it checks it itself, stops, replaces its
/// programs and starts again, with this agent).
pub fn apply_through_service(
    service: &mut crate::service_link::ServiceLink,
    installer: &Path,
    version: &str,
    manifest: &[u8],
    signature: &str,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    let own_installed = queqiao_core::platform::installation_folder()
        .join("queqiao-setup.exe")
        .is_file();
    if own_installed && failed_before(version) {
        lines.push(format!(
            "updates: this person's installation: {version} did not keep running here before: skipped"
        ));
    } else if own_installed {
        write_text("UpdateTried", version)?;
        let report = folder().join("update_report.txt");
        // At Medium integrity, as the person's own programs (this agent runs raised by the service): the installer and
        // the agent it starts lie in folders any program of theirs may change (security review of 2026-11-27).
        let arguments = format!(
            "--quiet --update --report {}",
            queqiao_core::platform::quote(&report.display().to_string())
        );
        let status = queqiao_core::platform::run_program_and_wait(
            installer,
            &arguments,
            std::time::Duration::from_secs(300),
        );
        lines.push(match status {
            Ok(Some(0)) => format!("updates: this person's installation is {version} now"),
            Ok(Some(code)) => {
                format!("updates: this person's installation was not updated (exit code {code})")
            }
            Ok(None) => {
                "updates: this person's installation: its installer still runs after 5 minutes"
                    .into()
            }
            Err(error) => format!("updates: this person's installation: {error}"),
        });
    }
    let bytes = std::fs::read(installer).map_err(|e| format!("{}: {e}", installer.display()))?;
    let reply = service.update(manifest, signature, bytes)?;
    lines.push(format!("updates: the Queqiao service: {reply}"));
    Ok(())
}

/// The update feed: Queqiao's own; tests point it elsewhere (builds with the `test-switches` feature only, so that a
/// program of the person cannot steer or stall the updates of a shipped Queqiao, as the security review of
/// 2026-11-22 found).
fn feed() -> String {
    if !cfg!(feature = "test-switches") {
        return queqiao_update::FEED.to_string();
    }
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
