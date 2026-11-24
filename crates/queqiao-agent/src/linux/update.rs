//! Updates on Linux (Windows: update.rs, the signed feed and the installer): not yet. Queqiao on Linux comes as a
//! package (.deb, docs/LINUX.md step 6) that the system's own updates keep current; until then the agent does not
//! look for updates (`installed_here` is false, so no Updater is started) and says so when asked.

use std::path::{Path, PathBuf};

/// All of Windows' answers, as the shared account mode handles them; Linux gives only Failed.
#[allow(dead_code)]
pub enum Found {
    UpToDate {
        by_hand: bool,
    },
    Ready {
        version: String,
        notes: String,
        installer: PathBuf,
        manifest: Vec<u8>,
        signature: String,
        by_hand: bool,
    },
    Failed {
        error: String,
        by_hand: bool,
    },
    Skipped {
        version: String,
        by_hand: bool,
    },
}

/// Never made on Linux (`installed_here` is false); it answers that updates come with the system's packages.
pub struct Updater {
    asked: std::cell::Cell<bool>,
}

impl Updater {
    pub fn start(lines: &mut Vec<String>, _through_service: bool) -> Updater {
        lines
            .push("updates: through the system's packages on Linux (not by Queqiao itself)".into());
        Updater {
            asked: std::cell::Cell::new(false),
        }
    }

    pub fn request(&self) {
        self.asked.set(true);
    }

    pub fn try_result(&self) -> Option<Found> {
        self.asked.replace(false).then(|| Found::Failed {
            error: "updates on Linux come with the system's packages".into(),
            by_hand: true,
        })
    }
}

/// Whether this copy was installed by Queqiao's installer (Windows); never on Linux yet.
pub fn installed_here() -> bool {
    false
}

pub fn apply(_installer: &Path, _version: &str) -> Result<(), String> {
    Err("updates on Linux come with the system's packages".into())
}

pub fn apply_through_service(
    _service: &mut crate::service_link::ServiceLink,
    _installer: &Path,
    _version: &str,
    _manifest: &[u8],
    _signature: &str,
    _lines: &mut Vec<String>,
) -> Result<(), String> {
    Err("updates on Linux come with the system's packages".into())
}
