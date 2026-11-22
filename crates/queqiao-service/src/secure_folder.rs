//! Folders that only SYSTEM and administrators may change, made so that nothing anyone else put there survives.
//!
//! Ordinary users may create folders under `%ProgramData%` and own what they create. The security review of
//! 2026-11-22 found that a folder made there before the service was installed (or in the moment between creating
//! the service's folder and securing it) stayed the user's: they could put a DLL next to the installer the service
//! runs as SYSTEM, or swap the installer. So a folder is never taken over: whatever is at the path is removed (a
//! junction or link as itself, never what it points to), and the folder is created anew with its final security in
//! one step (`CreateDirectoryW` with a security descriptor), so there is no moment in which others may write in it.

use std::os::windows::fs::MetadataExt;
use std::path::Path;

use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
    SetNamedSecurityInfoW,
};
use windows::Win32::Security::{
    GetSecurityDescriptorOwner, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES,
};
use windows::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
};
use windows::core::PCWSTR;

/// `%ProgramData%\Queqiao`: SYSTEM and administrators may change it, users may read it (the log).
pub const DATA: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";
/// Folders nobody but SYSTEM and administrators may even read (the installers of updates).
pub const PRIVATE: &str = "D:PAI(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)";

/// Makes `folder` new: removes whatever is at its path (nothing that a link points to), then creates it with the
/// security `sddl` from the start, owned by the Administrators group. Someone making it again in between makes this
/// try again, three times at most. Returns a note when something was removed.
pub fn fresh(folder: &Path, sddl: &str) -> Result<Option<String>, String> {
    let mut removed = None;
    for _ in 0..3 {
        if let Ok(meta) = std::fs::symlink_metadata(folder) {
            remove(folder, &meta)?;
            removed = Some(format!("{} was there before: removed", folder.display()));
        }
        match create(folder, sddl) {
            Ok(()) => {
                set_owner_administrators(folder)?;
                return Ok(removed);
            }
            Err(error) if error.code() == ERROR_ALREADY_EXISTS.to_hresult() => continue,
            Err(error) => {
                return Err(format!(
                    "creating {}: {}",
                    folder.display(),
                    error.message()
                ));
            }
        }
    }
    Err(format!(
        "{} is made again each time it is removed: something else on this PC keeps it",
        folder.display()
    ))
}

/// Removes `path` as it is: a junction or link itself (never what it points to), a folder with all in it (Rust's
/// `remove_dir_all` does not follow links inside either), or a file.
fn remove(path: &Path, meta: &std::fs::Metadata) -> Result<(), String> {
    let attributes = meta.file_attributes();
    let is_link = attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0;
    let is_folder = attributes & FILE_ATTRIBUTE_DIRECTORY.0 != 0;
    let result = match (is_link, is_folder) {
        (true, true) => std::fs::remove_dir(path),
        (false, true) => std::fs::remove_dir_all(path),
        (_, false) => std::fs::remove_file(path),
    };
    result.map_err(|e| format!("removing {}: {e}", path.display()))
}

fn create(folder: &Path, sddl: &str) -> windows::core::Result<()> {
    let descriptor = Descriptor::new(sddl)?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0.0,
        bInheritHandle: false.into(),
    };
    let path = wide(&folder.display().to_string());
    unsafe { CreateDirectoryW(PCWSTR(path.as_ptr()), Some(&attributes)) }
}

/// The Administrators group as the owner (an administrator's or SYSTEM's own account otherwise), as the folders the
/// service made before were.
fn set_owner_administrators(folder: &Path) -> Result<(), String> {
    let descriptor = Descriptor::new("O:BA").map_err(|e| e.message())?;
    let mut owner = PSID::default();
    let mut defaulted = false.into();
    unsafe { GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) }
        .map_err(|e| e.message())?;
    let path = wide(&folder.display().to_string());
    unsafe {
        SetNamedSecurityInfoW(
            PCWSTR(path.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(owner),
            None,
            None,
            None,
        )
    }
    .ok()
    .map_err(|e| format!("owner of {}: {}", folder.display(), e.message()))
}

/// A security descriptor made from SDDL, freed when dropped.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    fn new(sddl: &str) -> windows::core::Result<Descriptor> {
        let text = wide(sddl);
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(text.as_ptr()),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }?;
        Ok(Descriptor(descriptor))
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        unsafe { LocalFree(Some(HLOCAL(self.0.0))) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Security::Authorization::GetNamedSecurityInfoW;
    use windows::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
    };

    fn scratch(name: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("queqiao-secure-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn junction(link: &Path, target: &Path) {
        let status = std::process::Command::new("cmd.exe")
            .args(["/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(status.status.success(), "mklink /J failed");
    }

    fn protected(path: &Path) -> bool {
        let name = wide(&path.display().to_string());
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        let result = unsafe {
            GetNamedSecurityInfoW(
                PCWSTR(name.as_ptr()),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                &mut descriptor,
            )
        };
        assert!(result.is_ok());
        let mut control = 0u16;
        let mut revision = 0u32;
        unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) }.unwrap();
        unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
        control & SE_DACL_PROTECTED.0 != 0
    }

    #[test]
    fn what_was_there_goes_and_links_are_not_followed() {
        let base = scratch("folder");
        // Somewhere else, with a file that must survive.
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("keep.txt"), b"keep").unwrap();
        // The folder as someone else left it: a planted file, and a junction to the other folder.
        let folder = base.join("Queqiao");
        std::fs::create_dir_all(folder.join("updates")).unwrap();
        std::fs::write(folder.join("updates").join("planted.dll"), b"x").unwrap();
        junction(&folder.join("link"), &elsewhere);
        let note = fresh(&folder, PRIVATE).unwrap();
        assert!(note.is_some());
        assert_eq!(
            std::fs::read_dir(&folder).unwrap().count(),
            0,
            "the folder is new and empty"
        );
        assert!(protected(&folder), "its access is its own, not inherited");
        assert_eq!(std::fs::read(elsewhere.join("keep.txt")).unwrap(), b"keep");
        // A folder that is itself a junction: the junction goes, its target stays.
        let linked = base.join("linked");
        junction(&linked, &elsewhere);
        fresh(&linked, PRIVATE).unwrap();
        let meta = std::fs::symlink_metadata(&linked).unwrap();
        assert_eq!(meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT.0, 0);
        assert!(elsewhere.join("keep.txt").exists());
        // Nothing there: made, nothing to report.
        let new = base.join("new");
        assert_eq!(fresh(&new, DATA).unwrap(), None);
        assert!(new.is_dir() && protected(&new));
        let _ = std::fs::remove_dir_all(&base);
    }
}
