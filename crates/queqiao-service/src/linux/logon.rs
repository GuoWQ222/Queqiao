//! The device for the sign-in screen (Linux): what the person's agent hands over (this device's id, name and key, the
//! account login, the pins) while the person lets the account's devices in without asking. Kept in
//! `/var/lib/queqiao/logon.bin` (the service's state folder, root only; the file 0600), handed to the agent for the
//! sign-in screen over its channel, in memory only. Notices for the next person to sign in wait in `notices.txt`
//! beside it.

use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::PathBuf;

use queqiao_local::{FromService, LogonIdentity};

fn folder() -> PathBuf {
    PathBuf::from(super::STATE)
}

fn path() -> PathBuf {
    folder().join("logon.bin")
}

/// The state folder: root's to change, readable by all (the agents read update_tried.txt there); the private files
/// in it are 0600.
fn make_folder() -> Result<(), String> {
    let folder = folder();
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(&folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("{}: {e}", folder.display()))
}

/// Writes `bytes` to `name` in the state folder as a new file only root may read, then puts it in place.
fn write_private(name: &str, bytes: &[u8]) -> Result<(), String> {
    make_folder()?;
    let target = folder().join(name);
    let temporary = folder().join(format!(".{name}.new"));
    let _ = std::fs::remove_file(&temporary);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|e| format!("{}: {e}", temporary.display()))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|e| format!("{}: {e}", temporary.display()))?;
    std::fs::rename(&temporary, &target).map_err(|e| format!("{}: {e}", target.display()))
}

pub fn stored() -> bool {
    path().is_file()
}

pub fn save(identity: &LogonIdentity) -> Result<(), String> {
    // The same encoding as on the channel.
    write_private(
        "logon.bin",
        &FromService::Identity(identity.clone()).encode(),
    )
}

pub fn load() -> Option<LogonIdentity> {
    match FromService::decode(&std::fs::read(path()).ok()?)? {
        FromService::Identity(identity) => Some(identity),
        _ => None,
    }
}

/// Whether the account `offering` (its number) may hand over or take back the device kept for the sign-in screen:
/// only the one who handed it over (any account when none is kept). As on Windows (the security review of
/// 2026-11-22): a second user of the PC could otherwise put a device of their own account in its place.
pub fn may_hand_over(kept_owner: Option<&str>, offering: &str) -> bool {
    match kept_owner {
        None | Some("") => true,
        Some(owner) => owner == offering,
    }
}

pub fn forget() {
    let _ = std::fs::remove_file(path());
}

/// Keeps a notice for the next person to sign in (at most 20, each one line).
pub fn keep_notice(notice: &str) -> Result<(), String> {
    let mut notices = take_notices_peek();
    notices.push(notice.replace(['\n', '\r'], " "));
    let start = notices.len().saturating_sub(20);
    write_private("notices.txt", notices[start..].join("\n").as_bytes())
}

fn take_notices_peek() -> Vec<String> {
    std::fs::read_to_string(folder().join("notices.txt"))
        .map(|text| {
            text.lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The notices kept for the next person, removed.
pub fn take_notices() -> Vec<String> {
    let notices = take_notices_peek();
    let _ = std::fs::remove_file(folder().join("notices.txt"));
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_owner_hands_over_again() {
        assert!(may_hand_over(None, "1000"));
        assert!(may_hand_over(Some(""), "1000"));
        assert!(may_hand_over(Some("1000"), "1000"));
        assert!(!may_hand_over(Some("1000"), "1001"));
    }
}
