//! Updates of the service's own programs (`%ProgramFiles%\Queqiao`), asked for by the person's agent with what it
//! downloaded. Everything is checked here again, whatever the agent says: the manifest's signature by the release
//! key built into this service, the installer's size and SHA-256, and that it is a later version than this service.
//! Only then is the installer written where only SYSTEM and administrators may write (`%ProgramData%\Queqiao\updates`,
//! under the folder `install` secured) and started as SYSTEM with `--service-update`: it stops this service, replaces
//! the programs (keeping the earlier ones until the new ones work), starts the service again, and puts the earlier
//! programs back if the new agent does not keep running. A version tried that way is not tried again for a week
//! (queqiao_update::RETRY_AFTER) while this service is still older (`update_tried.txt` in `%ProgramData%\Queqiao`,
//! where only SYSTEM and administrators may write).

use std::os::windows::process::CommandExt;
use std::path::PathBuf;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const DETACHED_PROCESS: u32 = 0x0000_0008;

/// Checks the update and starts its installer; the version it installs.
pub fn start(manifest: &[u8], signature: &str, installer: &[u8]) -> Result<String, String> {
    let manifest = queqiao_update::verify_manifest(manifest, signature)?;
    queqiao_update::check_installer(&manifest, installer)?;
    if !queqiao_update::newer(&manifest.version, queqiao_core::VERSION) {
        return Err(format!(
            "{} is not later than this service ({})",
            manifest.version,
            queqiao_core::VERSION
        ));
    }
    // Still later than this service and tried in the last RETRY_AFTER: its installer put this one back.
    let tried = crate::log::folder().join("update_tried.txt");
    let now = queqiao_update::unix_now();
    if std::fs::read_to_string(&tried)
        .is_ok_and(|note| queqiao_update::tried_recently(&note, &manifest.version, now))
    {
        return Err(format!(
            "{} was tried here in the last week and did not keep running: not again yet",
            manifest.version
        ));
    }
    std::fs::write(&tried, queqiao_update::tried_note(&manifest.version, now))
        .map_err(|e| format!("{}: {e}", tried.display()))?;
    // Made new for each update, for SYSTEM and administrators only: nobody else can have put a DLL beside the
    // installer, or swap it, before it runs as SYSTEM (secure_folder).
    let folder: PathBuf = crate::log::folder().join("updates");
    crate::secure_folder::fresh(&folder, crate::secure_folder::PRIVATE)?;
    let path = folder.join(&manifest.installer);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, installer))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let report = folder.join("service_update.txt");
    std::process::Command::new(&path)
        .arg("--service-update")
        .arg("--report")
        .arg(&report)
        .current_dir(&folder)
        .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
        .spawn()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(manifest.version)
}
