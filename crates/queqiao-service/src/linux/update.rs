//! Updates of the installed package (Linux), asked for by the person's agent with what it downloaded. As on Windows,
//! everything is checked here again, whatever the agent says: the manifest's signature by the release key built into
//! this service, the package's size and SHA-256, that it is a later version, that it is this system's kind of package
//! (a .deb where dpkg installed Queqiao, an .rpm where rpm did) and that it is Queqiao's. Only then is it written
//! where only root may write (`/var/lib/queqiao/updates`, made new each time) and installed by
//! `queqiao-service install-update`, run by systemd as a unit of its own: installing the package restarts this
//! service, which would end anything it started. That unit waits for the new service (and its agent, when one ran)
//! to keep running, and otherwise puts back the package an earlier update installed. A version tried that way is not
//! tried again for a week (`update_tried.txt`, as on Windows).

use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use super::{RUNTIME, STATE, log};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Deb,
    Rpm,
}

impl Kind {
    fn extension(self) -> &'static str {
        match self {
            Kind::Deb => "deb",
            Kind::Rpm => "rpm",
        }
    }
}

/// How Queqiao is installed here: by dpkg or by rpm (None: not from a package).
fn installed_kind() -> Option<Kind> {
    if Path::new("/var/lib/dpkg/info/queqiao.list").is_file() {
        return Some(Kind::Deb);
    }
    let rpm = Command::new("rpm")
        .args(["-q", "--quiet", "queqiao"])
        .status();
    rpm.is_ok_and(|status| status.success())
        .then_some(Kind::Rpm)
}

/// The kind of package these bytes are, by their first bytes.
fn package_kind(bytes: &[u8]) -> Option<Kind> {
    if bytes.starts_with(b"!<arch>\ndebian-binary") {
        Some(Kind::Deb)
    } else if bytes.starts_with(&[0xed, 0xab, 0xee, 0xdb]) {
        Some(Kind::Rpm)
    } else {
        None
    }
}

/// The package's own name, as its tools read it.
fn package_name(kind: Kind, path: &Path) -> Result<String, String> {
    let output = match kind {
        Kind::Deb => Command::new("dpkg-deb")
            .arg("-f")
            .arg(path)
            .arg("Package")
            .output(),
        Kind::Rpm => Command::new("rpm")
            .args(["-qp", "--queryformat", "%{NAME}"])
            .arg(path)
            .output(),
    }
    .map_err(|e| format!("reading the package: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn updates_folder() -> PathBuf {
    Path::new(STATE).join("updates")
}

/// Checks the update and starts its installation; the version it installs.
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
    let tried = Path::new(STATE).join("update_tried.txt");
    let now = queqiao_update::unix_now();
    if std::fs::read_to_string(&tried)
        .is_ok_and(|note| queqiao_update::tried_recently(&note, &manifest.version, now))
    {
        return Err(format!(
            "{} was tried here in the last week and did not keep running: not again yet",
            manifest.version
        ));
    }
    let kind = installed_kind()
        .ok_or("Queqiao was not installed from a package here: nothing to update")?;
    if package_kind(installer) != Some(kind) {
        return Err(format!(
            "the update is not a .{} package, which this system installs",
            kind.extension()
        ));
    }
    let name = &manifest.installer;
    if name.contains('/')
        || name.starts_with('.')
        || !name.ends_with(&format!(".{}", kind.extension()))
    {
        return Err(format!(
            "the update's file name {name:?} is not a package's"
        ));
    }
    // Made new for each update, root's only: nothing else can have changed it before it is installed.
    let folder = updates_folder();
    let _ = std::fs::remove_dir_all(&folder);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(|e| format!("{}: {e}", folder.display()))?;
    let path = folder.join(name);
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, installer))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let package = package_name(kind, &path)?;
    if package != "queqiao" {
        return Err(format!(
            "the update is the package {package:?}, not Queqiao"
        ));
    }
    std::fs::write(&tried, queqiao_update::tried_note(&manifest.version, now))
        .map_err(|e| format!("{}: {e}", tried.display()))?;
    let program = std::env::current_exe().map_err(|e| format!("this program: {e}"))?;
    let status = Command::new("systemd-run")
        .arg(format!("--unit=queqiao-update-{now}"))
        .args(["--collect", "--quiet", "--description=Queqiao update"])
        .arg(&program)
        .arg("install-update")
        .arg(&path)
        .arg(&manifest.version)
        .status()
        .map_err(|e| format!("systemd-run: {e}"))?;
    if !status.success() {
        return Err(format!("systemd-run: {status}"));
    }
    Ok(manifest.version)
}

fn install_package(kind: Kind, path: &Path) -> Result<(), String> {
    let output = match kind {
        Kind::Deb => Command::new("dpkg")
            .arg("-i")
            .arg(path)
            .env("DEBIAN_FRONTEND", "noninteractive")
            .output(),
        // --oldpackage: putting an earlier one back.
        Kind::Rpm => Command::new("rpm")
            .args(["-U", "--oldpackage", "--replacepkgs"])
            .arg(path)
            .output(),
    }
    .map_err(|e| format!("{}: {e}", path.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let error = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "installing {}: {}",
        path.display(),
        error
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("failed")
    ))
}

fn status_value(text: &str, name: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix(&format!("{name}=")))
        .map(str::to_string)
}

/// Whether the service of `version` runs, with an agent that has run for 20 s when `agent_expected`.
fn keeps_running(version: &str, agent_expected: bool) -> bool {
    let Ok(text) = std::fs::read_to_string(Path::new(RUNTIME).join("status")) else {
        return false;
    };
    if status_value(&text, "version").as_deref() != Some(version) {
        return false;
    }
    if !agent_expected {
        return true;
    }
    let alive = status_value(&text, "agent_pid")
        .and_then(|pid| pid.parse::<i32>().ok())
        .is_some_and(|pid| unsafe { libc::kill(pid, 0) } == 0);
    let since = status_value(&text, "agent_since").and_then(|s| s.parse::<u64>().ok());
    alive && since.is_some_and(|since| queqiao_update::unix_now().saturating_sub(since) >= 20)
}

/// `install-update <package> <version>`: installs, waits up to three minutes for the new service (and its agent, if
/// one ran before) to keep running, and puts back the package an earlier update installed otherwise.
pub fn install(args: &[String]) -> Result<(), String> {
    let [path, version] = args else {
        return Err(
            "usage: queqiao-service install-update <package> <version> (the service starts it)"
                .into(),
        );
    };
    if unsafe { libc::geteuid() } != 0 {
        return Err("install-update runs as root (the service starts it)".into());
    }
    let path = PathBuf::from(path);
    if path.parent() != Some(updates_folder().as_path()) {
        return Err(format!(
            "{} is not in the service's update folder",
            path.display()
        ));
    }
    let kind = package_kind(&std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?)
        .ok_or("not a package")?;
    let before = std::fs::read_to_string(Path::new(RUNTIME).join("status")).unwrap_or_default();
    let agent_expected = status_value(&before, "agent_pid").is_some();
    let kept = Path::new(STATE).join(format!("installed.{}", kind.extension()));
    log::line(&format!(
        "update: installing {} ({version})",
        path.display()
    ));
    let installed = install_package(kind, &path);
    let works = installed.is_ok() && {
        let until = Instant::now() + Duration::from_secs(180);
        loop {
            if keeps_running(version, agent_expected) {
                break true;
            }
            if Instant::now() >= until {
                break false;
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    };
    if works {
        let _ = std::fs::copy(&path, &kept);
        log::line(&format!("update: {version} installed and keeps running"));
        return Ok(());
    }
    let why = match installed {
        Err(error) => error,
        Ok(()) => format!("{version} did not keep running"),
    };
    if kept.is_file() {
        let back = install_package(kind, &kept);
        log::line(&format!(
            "update: {why}; the earlier package {}",
            match back {
                Ok(()) => "was put back".to_string(),
                Err(error) => format!("could not be put back: {error}"),
            }
        ));
    } else {
        log::line(&format!(
            "update: {why}; no earlier package kept here to put back"
        ));
    }
    Err(why)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_told_apart_by_their_first_bytes() {
        assert_eq!(package_kind(b"!<arch>\ndebian-binary   1"), Some(Kind::Deb));
        assert_eq!(
            package_kind(&[0xed, 0xab, 0xee, 0xdb, 3, 0]),
            Some(Kind::Rpm)
        );
        assert_eq!(package_kind(b"MZ\x90\0"), None);
        assert_eq!(
            status_value("version=1.2.3\npid=4\n", "version").as_deref(),
            Some("1.2.3")
        );
        assert_eq!(status_value("version=1.2.3\n", "agent_pid"), None);
    }
}
