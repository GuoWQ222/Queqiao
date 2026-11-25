//! Updates on Linux (docs/LINUX.md): Queqiao comes as a package (.deb or .rpm) that root installs, so only the Queqiao
//! service updates it. The person's agent it started looks at the update feed two minutes after it starts and then
//! once a day (or when the person asks), on a thread of its own: the manifest for this system's kind of package
//! (queqiao_update::MANIFEST_DEB or MANIFEST_RPM), signed by the release key, a later version; the package is
//! downloaded into the person's cache (`~/.cache/queqiao/updates`) and checked, and handed to the service between
//! sessions, which checks all of it again itself and installs it (and puts the earlier package back if the new one
//! does not keep running). A version the service tried is not tried again for a week (its `update_tried.txt`, which
//! the agent reads). The newest manifest seen (`~/.config/queqiao/update_seen.txt`) keeps an older feed served again
//! from being taken.
//!
//! Tests point it elsewhere (builds with the `test-switches` feature only): `~/.config/queqiao/update.conf` with
//! `feed=<address>` (HTTPS, or HTTP on this PC) and `first_check_seconds=<n>`.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

const DAY: Duration = Duration::from_secs(24 * 3600);
/// After a failed check (no network, the feed unreachable), the next one.
const AFTER_FAILURE: Duration = Duration::from_secs(3600);
/// What the service notes of the versions it tried (readable by all).
const SERVICE_TRIED: &str = "/var/lib/queqiao/update_tried.txt";

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
    /// Only with the service (`through_service`): it alone installs packages.
    pub fn start(lines: &mut Vec<String>, through_service: bool) -> Updater {
        let (requests, wanted) = mpsc::channel::<()>();
        let (found, results) = mpsc::channel();
        let feed = feed();
        let first_check = if cfg!(feature = "test-switches") {
            setting("first_check_seconds").and_then(|value| value.parse::<u64>().ok())
        } else {
            None
        };
        let first = Duration::from_secs(first_check.unwrap_or(120));
        let manifest = package_manifest();
        lines.push(match (manifest, through_service) {
            (Some(manifest), true) => format!(
                "updates: {manifest} from {feed}, first look in {} s",
                first.as_secs()
            ),
            (None, _) => "updates: Queqiao was not installed from a package here: none".into(),
            (_, false) => "updates: through the Queqiao service only (it is not running)".into(),
        });
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
                    let result = match (manifest, through_service) {
                        (Some(manifest), true) => look(&feed, manifest, by_hand),
                        (None, _) => Found::Failed {
                            error: "Queqiao was not installed from a package on this PC".into(),
                            by_hand,
                        },
                        (_, false) => Found::Failed {
                            error: "updates go through the Queqiao service, which does not run"
                                .into(),
                            by_hand,
                        },
                    };
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

/// The manifest for how Queqiao is installed here (dpkg or rpm); None when not from a package.
fn package_manifest() -> Option<&'static str> {
    if Path::new("/var/lib/dpkg/info/queqiao.list").is_file() {
        return Some(queqiao_update::MANIFEST_DEB);
    }
    let rpm = std::process::Command::new("rpm")
        .args(["-q", "--quiet", "queqiao"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    rpm.is_ok_and(|status| status.success())
        .then_some(queqiao_update::MANIFEST_RPM)
}

fn config_folder() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
    Some(base.join("queqiao"))
}

fn look(feed: &str, manifest: &str, by_hand: bool) -> Found {
    let seen_file = config_folder().map(|folder| folder.join("update_seen.txt"));
    let newest_seen = seen_file
        .as_ref()
        .and_then(|file| std::fs::read_to_string(file).ok())
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let checked =
        queqiao_update::check_manifest(feed, manifest, queqiao_core::VERSION, newest_seen);
    if let Ok(checked) = &checked
        && checked.published > newest_seen
        && let Some(file) = &seen_file
    {
        if let Some(folder) = file.parent() {
            let _ = std::fs::create_dir_all(folder);
        }
        let _ = std::fs::write(file, checked.published.to_string());
    }
    match checked.map(|checked| checked.offer) {
        Ok(None) => Found::UpToDate { by_hand },
        Ok(Some(offer)) if tried_before(&offer.manifest.version) => Found::Skipped {
            version: offer.manifest.version,
            by_hand,
        },
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

/// Where downloaded packages go.
fn folder() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("queqiao")
        .join("updates")
}

/// Never: a person's agent does not install packages itself (root does, through the service).
pub fn installed_here() -> bool {
    false
}

/// Whether the service tried `version` lately and it did not keep running.
fn tried_before(version: &str) -> bool {
    std::fs::read_to_string(SERVICE_TRIED).is_ok_and(|note| {
        queqiao_update::tried_recently(&note, version, queqiao_update::unix_now())
            && queqiao_update::newer(version, queqiao_core::VERSION)
    })
}

pub fn apply(_installer: &Path, _version: &str) -> Result<(), String> {
    Err("on Linux, updates are installed by the Queqiao service".into())
}

/// Hands the package to the service (it checks it itself, installs it, which restarts the service with this agent).
pub fn apply_through_service(
    service: &mut crate::service_link::ServiceLink,
    installer: &Path,
    _version: &str,
    manifest: &[u8],
    signature: &str,
    lines: &mut Vec<String>,
) -> Result<(), String> {
    let bytes = std::fs::read(installer).map_err(|e| format!("{}: {e}", installer.display()))?;
    let reply = service.update(manifest, signature, bytes)?;
    lines.push(format!("updates: the Queqiao service: {reply}"));
    Ok(())
}

/// A test setting (test builds only) from `~/.config/queqiao/update.conf`.
fn setting(name: &str) -> Option<String> {
    let text = std::fs::read_to_string(config_folder()?.join("update.conf")).ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(|value| value.trim().to_string())
}

/// The update feed: Queqiao's own; tests point it elsewhere (test builds only, so that a program of the person cannot
/// steer or stall the updates of a shipped Queqiao).
fn feed() -> String {
    if !cfg!(feature = "test-switches") {
        return queqiao_update::FEED.to_string();
    }
    setting("feed")
        .filter(|feed| queqiao_update::check_feed(feed).is_ok())
        .unwrap_or_else(|| queqiao_update::FEED.to_string())
}
