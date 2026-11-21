//! Updates of the service's own programs (`%ProgramFiles%\Queqiao`), asked for by the person's agent with what it
//! downloaded. Everything is checked here again, whatever the agent says: the manifest's signature by the release
//! key built into this service, the installer's size and SHA-256, and that it is a later version than this service.
//! Only then is the installer written where only SYSTEM and administrators may write (`%ProgramData%\Queqiao\updates`,
//! under the folder `install` secured) and started as SYSTEM with `--service-update`: it stops this service, replaces
//! the programs (keeping the earlier ones until the new ones work), starts the service again, and puts the earlier
//! programs back if the new agent does not keep running.

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
    let folder: PathBuf = crate::log::folder().join("updates");
    std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(&manifest.installer);
    std::fs::write(&path, installer).map_err(|e| format!("{}: {e}", path.display()))?;
    let report = folder.join("service_update.txt");
    let _ = std::fs::remove_file(&report);
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
